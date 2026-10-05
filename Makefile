.PHONY: all build test lint fmt install run e2e clean
all: build
build:
	cargo build --release --locked
test:
	cargo test --all-targets --locked
lint:
	cargo clippy --all-targets --locked -- -D warnings
fmt:
	cargo fmt --all
install:
	cargo install --path . --locked
run:
	cargo run --locked -- serve
e2e:
	cargo test --test protocol_smoke --locked
clean:
	cargo clean
