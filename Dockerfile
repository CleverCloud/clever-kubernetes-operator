FROM redhat/ubi9:latest AS builder

WORKDIR /usr/src/clever-kubernetes-operator
ADD src src
ADD Cargo.toml .
ADD Cargo.lock .

RUN dnf update -y && dnf install gcc openssl-devel -y
RUN curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- --verbose -y \
    && export PATH="$HOME/.cargo/bin:$PATH" \
    && cargo build --release

FROM redhat/ubi9:latest

LABEL name="clever-kubernetes-operator" \
    maintainer="Florentin Dubois <florentin.dubois@clever-cloud.com>" \
    vendor="Clever Cloud S.A.S" \
    version="v0.6.0" \
    release="1" \
    summary="A kubernetes operator that expose clever cloud's resources through custom resource definition" \
    description="A kubernetes operator that expose clever cloud's resources through custom resource definition"

RUN groupadd -g 25000 clever && useradd -u 20000 clever -g clever
USER clever:clever

COPY --from=builder /usr/src/clever-kubernetes-operator/target/release/clever-kubernetes-operator /usr/local/bin
ADD LICENSE licenses/LICENSE
CMD [ "/usr/local/bin/clever-kubernetes-operator" ]
