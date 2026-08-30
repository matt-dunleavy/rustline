
# Variables
BINARY_NAME = rustline
INSTALL_DIR = /home/matt/bin
CARGO = cargo
TARGET_DIR = target
RELEASE_DIR = $(TARGET_DIR)/release

# Default target
.PHONY: all
all: build

# Build the project in release mode
.PHONY: build
build:
	@echo "Building $(BINARY_NAME) in release mode..."
	$(CARGO) build --release
	@cp $(RELEASE_DIR)/$(BINARY_NAME) bin/$(BINARY_NAME)

# Build the project in debug mode
.PHONY: debug
debug:
	@echo "Building $(BINARY_NAME) in debug mode..."
	$(CARGO) build

# Install the binary to the specified directory
.PHONY: install
install: build
	@echo "Installing $(BINARY_NAME) to $(INSTALL_DIR)..."
	@mkdir -p $(INSTALL_DIR)
	@cp $(RELEASE_DIR)/$(BINARY_NAME) $(INSTALL_DIR)/$(BINARY_NAME)
	@chmod +x $(INSTALL_DIR)/$(BINARY_NAME)
	@echo "Successfully installed $(BINARY_NAME) to $(INSTALL_DIR)"
	@echo "Make sure $(INSTALL_DIR) is in your PATH to use $(BINARY_NAME) globally"

# Uninstall the binary from the specified directory
.PHONY: uninstall
uninstall:
	@echo "Uninstalling $(BINARY_NAME) from $(INSTALL_DIR)..."
	@if [ -f $(INSTALL_DIR)/$(BINARY_NAME) ]; then \
		rm $(INSTALL_DIR)/$(BINARY_NAME); \
		echo "Successfully uninstalled $(BINARY_NAME)"; \
	else \
		echo "$(BINARY_NAME) is not installed in $(INSTALL_DIR)"; \
	fi

# Clean build artifacts
.PHONY: clean
clean:
	@echo "Cleaning build artifacts..."
	$(CARGO) clean
	rm -rf bin/$(BINARY_NAME)
	rm -rf $(INSTALL_DIR)/$(BINARY_NAME)

# Run tests
.PHONY: test
test:
	@echo "Running tests..."
	$(CARGO) test

# Run the binary (debug mode)
.PHONY: run
run:
	@echo "Running $(BINARY_NAME) in debug mode..."
	$(CARGO) run

# Check code without building
.PHONY: check
check:
	@echo "Checking code..."
	$(CARGO) check

# Format code
.PHONY: fmt
format:
	@echo "Formatting code..."
	$(CARGO) fmt

# Run clippy lints
.PHONY: clippy
clippy:
	@echo "Running clippy..."
	$(CARGO) clippy

# Show help
.PHONY: help
help:
	@echo "Available targets:"
	@echo "  all       - Build the project (default)"
	@echo "  build     - Build the project in release mode"
	@echo "  debug     - Build the project in debug mode"
	@echo "  install   - Build and install binary to $(INSTALL_DIR)"
	@echo "  uninstall - Remove binary from $(INSTALL_DIR)"
	@echo "  clean     - Clean build artifacts"
	@echo "  test      - Run tests"
	@echo "  run       - Run the binary in debug mode"
	@echo "  check     - Check code without building"
	@echo "  format    - Format code with rustfmt"
	@echo "  clippy    - Run clippy lints"
	@echo "  help      - Show this help message"
