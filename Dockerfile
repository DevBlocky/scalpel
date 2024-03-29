FROM debian:buster AS build-stage
WORKDIR /build

# install required packages
# ENV TZ=America/Los_Angeles
# RUN ln -snf /usr/share/zoneinfo/$TZ /etc/localtime && echo $TZ > /etc/timezone
RUN apt-get update && apt-get install -y apt-transport-https
RUN apt-get install -y\
    curl\
    wget\
    lsb-release\
    software-properties-common\
    build-essential\
    libssl-dev\
    pkg-config\
    dos2unix

# install rust toolchain
ENV RUSTUP_HOME=/usr/local/rustup \
    CARGO_HOME=/usr/local/cargo \
    PATH=/usr/local/cargo/bin:$PATH
RUN curl https://sh.rustup.rs -sSf | sh -s -- -y --profile minimal

# install LLVM
RUN curl https://apt.llvm.org/llvm.sh | bash

# copy and build
COPY Cargo.toml Cargo.lock ./
COPY src src
RUN cargo build --release

# convert docker entrypoint to LF
COPY docker-entrypoint.sh ./
RUN dos2unix docker-entrypoint.sh


# copy stuff from build stage
FROM debian:buster
WORKDIR /mangahome

# needed for SSL to work right
RUN apt-get update && apt-get install -y ca-certificates

COPY --from=build-stage /build/target/release/scalpel ./
COPY --from=build-stage /build/docker-entrypoint.sh ./
RUN chmod +x scalpel docker-entrypoint.sh

STOPSIGNAL SIGTERM
ENTRYPOINT "./docker-entrypoint.sh"
CMD "./scalpel"
