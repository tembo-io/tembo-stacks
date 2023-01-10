FROM rust:1.66.0
COPY . .
RUN cargo build --release
ENV RUST_LOG=debug
CMD ["./target/release/reconciler"]