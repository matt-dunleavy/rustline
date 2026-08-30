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

## Everything CI runs.
.PHONY: ci
ci: fmt-check clippy test doc

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
