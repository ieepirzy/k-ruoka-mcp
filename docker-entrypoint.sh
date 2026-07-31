#!/bin/sh
# Makes `login`'s debug port reachable from outside the container.
#
# Chrome's remote-debugging HTTP server validates the actual TCP peer of a connection,
# not just its listen address or Host header -- so `--remote-debugging-address=0.0.0.0`
# alone does not help: a connection arriving through Docker's port-publish NAT still
# gets refused, because its peer address is not literally loopback. Verified directly:
# with Chrome bound to 0.0.0.0, a request from the host still failed with "connection
# reset by peer"; only a same-network-namespace relay resolved it.
#
# socat is the relay. It runs in this same container (same netns as Chrome), listens
# on the container's own address, and opens a *fresh* connection to Chrome's own
# 127.0.0.1 -- which Chrome accepts, because that connection genuinely originates from
# loopback. Chrome itself stays on its safe default; nothing here widens what Chrome
# will bind to.
#
# Binding the container's own address rather than 0.0.0.0: also verified directly,
# binding the wildcard address to the same port number Chrome already holds on
# 127.0.0.1 fails outright ("Address in use") on this network stack, wildcard and
# loopback binds are not as independent as they are on a bare host. The container's own
# address is a different, specific address, so it does not collide -- and it is what
# Docker's port-publish actually forwards to regardless.
#
# The wait matters, not just the relay: `login` refuses to start if its debug port is
# already taken (guarding against a leftover Chrome from a previous run), and it checks
# *before* Chrome launches. Starting the relay immediately would trip that guard against
# itself. Waiting for Chrome to actually be listening first means the port is still
# genuinely free at the moment `login` checks it.
set -eu

if [ "${1:-}" = "login" ]; then
    port=9222
    prev=""
    for arg in "$@"; do
        case "$prev" in
        --port) port="$arg" ;;
        esac
        case "$arg" in
        --port=*) port="${arg#--port=}" ;;
        esac
        prev="$arg"
    done
    container_ip=$(hostname -i 2>/dev/null || true)
    if [ -n "$container_ip" ]; then
        (
            for _ in $(seq 1 120); do
                if wget -q --spider "http://127.0.0.1:${port}/json/version" 2>/dev/null; then
                    exec socat "TCP-LISTEN:${port},bind=${container_ip},fork,reuseaddr" \
                        "TCP:127.0.0.1:${port}"
                fi
                sleep 0.5
            done
            echo "docker-entrypoint: Chrome's debug port never came up; the relay was not started" >&2
        ) &
    else
        echo "docker-entrypoint: could not determine this container's address; the debug port will only be reachable from inside the container (docker exec)" >&2
    fi
fi

exec k-ruoka-mcp "$@"
