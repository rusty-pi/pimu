# The image .github/workflows/release.yml publishes as ghcr.io/rusty-pi/pimu,
# one manifest per architecture. The binaries are built before it, against the
# glibc of the base below, so nothing compiles here and the arm64 image is
# assembled without emulation.
FROM debian:12-slim

# `boot <url>` fetches the boot partition's files over HTTP through curl, which
# is how a container boots without a bind mount at all.
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates curl \
 && rm -rf /var/lib/apt/lists/*

ARG TARGETARCH
COPY dist/pimu-$TARGETARCH /usr/local/bin/pimu

# A boot reads its firmware files from the working directory, so a bind mount
# there is the whole invocation:
#   docker run --rm -v "$PWD:/boot" ghcr.io/rusty-pi/pimu boot
# ...or from a URL, with nothing mounted:
#   docker run --rm ghcr.io/rusty-pi/pimu boot \
#     https://raw.githubusercontent.com/raspberrypi/firmware/refs/heads/master/boot/
WORKDIR /boot

ENTRYPOINT ["/usr/local/bin/pimu"]
