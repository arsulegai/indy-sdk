FROM amazonlinux:2017.03

ARG uid=1000

RUN \
    yum clean all \
    && yum upgrade -y \
    && yum groupinstall -y "Development Tools" \
    && yum install -y \
           wget \
           cmake \
           pkgconfig \
           openssl-devel \
           sqlite-devel

RUN \
    wget https://dl.fedoraproject.org/pub/epel/epel-release-latest-7.noarch.rpm \
    && rpm -Uvh epel-release-latest-7*.rpm \
    && yum install -y libsodium-devel

ENV RUST_ARCHIVE=rust-1.16.0-x86_64-unknown-linux-gnu.tar.gz
ENV RUST_DOWNLOAD_URL=https://static.rust-lang.org/dist/$RUST_ARCHIVE

RUN mkdir -p /rust
WORKDIR /rust

RUN curl -fsOSL $RUST_DOWNLOAD_URL \
    && curl -s $RUST_DOWNLOAD_URL.sha256 | sha256sum -c - \
    && tar -C /rust -xzf $RUST_ARCHIVE --strip-components=1 \
    && rm $RUST_ARCHIVE \
    && ./install.sh

ENV PATH="/usr/local/sbin:/usr/local/bin:/usr/sbin:/usr/bin:/sbin:/bin:/root/.cargo/bin"

RUN useradd -ms /bin/bash -u $uid sovrin
USER sovrin

WORKDIR /home/sovrin
