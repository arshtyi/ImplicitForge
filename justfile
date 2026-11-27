default: list

list:
    @just --list

[windows]
set shell := ["cmd.exe", "/d", "/c"]

[unix]
[positional-arguments]
run *args:
    #!/bin/sh
    # WSLg presents shared-memory buffers. Skip the EGL DRM-screen probe while
    # explicitly running GPU commands through Mesa's hardware D3D12 driver.
    if [ -c /dev/dxg ] && [ -n "${WSL_DISTRO_NAME:-}" ] && \
       [ -z "${GALLIUM_DRIVER+x}${MESA_LOADER_DRIVER_OVERRIDE+x}${LIBGL_ALWAYS_SOFTWARE+x}${WGPU_BACKEND+x}" ]; then
        export LIBGL_ALWAYS_SOFTWARE=true GALLIUM_DRIVER=d3d12 MESA_LOADER_DRIVER_OVERRIDE=d3d12
        # An empty substring selects the first DXCore adapter in enumeration order.
        export MESA_D3D12_DEFAULT_ADAPTER_NAME="${MESA_D3D12_DEFAULT_ADAPTER_NAME-}"
    fi
    exec cargo run --locked --release -- "$@"

[windows]
[positional-arguments]
[script("cmd.exe", "/d", "/c")]
[extension(".cmd")]
run *args:
    @cargo run --locked --release -- %*

fmt:
    cargo fmt --all --check

lint:
    cargo clippy --locked --all-targets -- -D warnings

test:
    cargo test --locked --all-targets

[unix]
[positional-arguments]
bench *args:
    cargo run --locked --release --bin measure -- "$@"

[windows]
[positional-arguments]
[script("cmd.exe", "/d", "/c")]
[extension(".cmd")]
bench *args:
    @cargo run --locked --release --bin measure -- %*
