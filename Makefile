# Rustline is a library crate; there is no binary to install. These targets are
# the development tasks, matching what CI runs.

CARGO ?= cargo

.PHONY: all
all: check

## Compile the library, examples and tests without running anything.
.PHONY: check
check:
	$(CARGO) check --all-targets --all-features

## Build in release mode.
.PHONY: build
build:
	$(CARGO) build --release --all-targets

# No separate build step: `cargo test` compiles the examples as test targets
# without refreshing the example binaries the pseudoterminal suite executes, so
# the suite builds those itself. See `example_path` in tests/common/mod.rs.
## Run every test, including the pseudoterminal integration suite.
.PHONY: test
test:
	$(CARGO) test --all-targets --all-features

## Run the demonstration REPL.
.PHONY: run
run:
	$(CARGO) run --example repl

## Check formatting without changing anything.
.PHONY: fmt-check
fmt-check:
	$(CARGO) fmt --all -- --check

## Reformat the source.
.PHONY: fmt
fmt:
	$(CARGO) fmt --all

## Lint, treating warnings as errors.
.PHONY: clippy
clippy:
	$(CARGO) clippy --all-targets --all-features -- -D warnings

## Build the API documentation.
.PHONY: doc
doc:
	$(CARGO) doc --no-deps --all-features

## Audit dependencies for advisories, licences and sources.
.PHONY: deny
deny:
	$(CARGO) deny check

## List what would ship and build the crate exactly as `cargo publish` would.
.PHONY: package
package:
	$(CARGO) package --list
	$(CARGO) package

# Not part of `ci`: it needs the 1.85.0 toolchain installed, which a working
# copy will not always have. RUSTUP_TOOLCHAIN is what actually pins the
# version; `rustup run` would be overridden by a rust-toolchain.toml.
## Check that the crate still builds on the minimum supported Rust version.
.PHONY: msrv
msrv:
	RUSTUP_TOOLCHAIN=1.85.0 $(CARGO) check --all-features

## Everything CI runs, apart from the MSRV check and the dependency audit.
.PHONY: ci
ci: fmt-check clippy test doc package

## Remove build artifacts.
.PHONY: clean
clean:
	$(CARGO) clean

## List the available targets.
.PHONY: help
help:
	@awk '/^## /{ doc = substr($$0, 4); next } \
	      /^[a-z][a-z-]*:/ && doc { \
	          split($$1, t, ":"); printf "  make %-12s %s\n", t[1], doc; doc = "" \
	      }' $(MAKEFILE_LIST)
