FROM rust:1.83.0-bookworm

RUN apt-get update && apt-get install -y git ssh lsof

ENV CARGO_REGISTRIES_CRATES_IO_PROTOCOL=sparse

RUN rustup component add rustfmt
RUN rustup component add clippy

WORKDIR /hyper-client-pool

CMD bash
