FROM quay.io/coredb/rust:1.71.0-slim-buster

RUN apt-get install -y pkg-config libssl-dev

WORKDIR /build

ENV CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse

COPY . .

RUN cargo build --release

FROM quay.io/coredb/rust:1.71.0-slim-buster

RUN apt-get install -y pkg-config libssl-dev

COPY --from=builder /build/target/release/dataplane_webserver /usr/local/bin/dataplane_webserver

CMD ["/usr/local/bin/dataplane_webserver"]
