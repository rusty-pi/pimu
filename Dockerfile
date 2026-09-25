# The image .github/workflows/release.yml publishes as ghcr.io/rusty-pi/pimu,
# one manifest per architecture. The binaries are built before it, against the
# glibc of the base below, so nothing compiles here and the arm64 image is
# assembled without emulation.
FROM debian:12-slim

ARG TARGETARCH
COPY dist/pimu-$TARGETARCH /usr/local/bin/pimu

# A boot reads its firmware files from the working directory, so a bind mount
# there is the whole invocation:
#   docker run --rm -v "$PWD:/boot" ghcr.io/rusty-pi/pimu boot
WORKDIR /boot

ENTRYPOINT ["/usr/local/bin/pimu"]
