# SPDX-License-Identifier: Apache-2.0
#
# The nri-hft e2e image: io_uring_probe, statically linked, on a small Debian
# base that already has sh, sleep and chrt (util-linux).
#
# Build from the nri-hft directory (the e2e script does this):
#   docker build -f test/Dockerfile -t ergon-hft-demo:e2e .

FROM gcc:14-trixie AS tools
WORKDIR /src
COPY tools/io_uring_probe.c ./
RUN mkdir -p /out \
 && gcc -O2 -static -o /out/io_uring_probe io_uring_probe.c

FROM debian:trixie-slim
COPY --from=tools /out/io_uring_probe /usr/local/bin/io_uring_probe
CMD ["sleep", "3600"]
