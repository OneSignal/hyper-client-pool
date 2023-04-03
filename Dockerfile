FROM rust:1-buster

RUN apt-get update && apt-get install -y git ssh lsof

WORKDIR /hyper-client-pool

CMD bash
