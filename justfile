# The recipes of the project. Run `just` to see the list of them.

# The version of the project, taken from Cargo.toml.
version := `grep -m1 -E '^version[[:space:]]*=' Cargo.toml | sed -E 's/.*"([^"]+)".*/\1/'`

# The architecture of the native packages.
arch := env_var_or_default("ARCH", "amd64")

# The default recipe: the same checks that the CI runs.
default: lint test

# Build the project.
build:
    cargo build --locked

# Build the optimized binary.
release:
    cargo build --release --locked

# Run the tests.
test:
    cargo test --locked

# Run the tests of the end-to-end suite only.
test-e2e:
    cargo test --locked --test e2e

# Run the tests of the unit suite only.
test-unit:
    cargo test --locked --lib

# Run a single test, like `just test-one calls_a_unary_rpc`.
test-one name:
    cargo test --locked {{name}}

# Type-check the project.
check:
    cargo check --all-targets --locked

# Run the linter.
lint:
    cargo clippy --all-targets --locked -- -D warnings

# Format the code.
fmt:
    cargo fmt

# Check the formatting.
fmt-check:
    cargo fmt --check

# Run all the checks of the CI.
ci: fmt-check lint test

# Run the REPL against the test proto, like `just repl --package test`.
repl *args:
    cargo run --locked -- --proto testdata/proto/test.proto {{args}}

# Call a method of the test server, like `just call test.APIService.Simple`.
call method *args:
    echo '{"name": "ktr"}' | cargo run --locked -- --proto testdata/proto/test.proto cli call {{method}} {{args}}

# Install the binary.
install:
    cargo install --path . --locked

# Remove the build artifacts.
clean:
    cargo clean

# Bump the version of the project, like `just bump 0.3.0`.
#
# The lock file is updated as well, so it is not locked.
bump version:
    sed -i -E 's/^version[[:space:]]*=.*/version = "{{version}}"/' Cargo.toml
    cargo check

# Build the native packages. The packager is one of deb, rpm and archlinux.
#
# It needs nfpm, which the CI installs.
[script]
package packager="deb": release
    #!/usr/bin/env bash
    set -euo pipefail

    mkdir -p dist/bin dist/share
    cp target/release/evans dist/bin/evans
    cp README.md dist/share/README.md
    if [ -f LICENSE ]; then
        cp LICENSE dist/share/LICENSE
    else
        sed -n '/^## License/,$p' README.md > dist/share/LICENSE
    fi

    case "$packager" in
        deb) target="dist/evans_${version}_${arch}.deb" ;;
        rpm) target="dist/evans-${version}-1.x86_64.rpm" ;;
        archlinux) target="dist/evans-${version}-1-x86_64.pkg.tar.zst" ;;
        *) echo "unknown packager: $packager" >&2; exit 1 ;;
    esac

    VERSION="$version" ARCH="$arch" nfpm package \
        --packager "$packager" \
        --config packaging/nfpm.yaml \
        --target "$target"
    echo "$target"
