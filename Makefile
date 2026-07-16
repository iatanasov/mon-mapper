.DEFAULT_GOAL	:= help
help: # Print this message
	@grep -h "#" $(MAKEFILE_LIST) | grep -v grep | sed -e 's/\\$$//' | sed -e 's/#//'

build: # Build in debug mode
	@cargo build

release: # Build in release mode
	@cargo build --release

install: # Install the binary locally
	@cargo install --profile release --path .

test: # Run tests
	@cargo test

lint: # Run clippy with warnings as errors
	@cargo clippy -- -D warnings

clean: # Remove build artifacts
	@cargo clean
