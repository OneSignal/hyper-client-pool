FROM osig/rust-ubuntu:1.26

RUN apt-get update
RUN apt-get install -y ssh git

WORKDIR /hyper-client-pool

CMD bash
