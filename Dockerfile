# Spool in a container: the server, its web app, and the helper tools it calls.
#
#   docker build -t spool .
#   docker run -p 7979:7979 -v spool-data:/data -v /path/to/media:/media spool

FROM oven/bun:1 AS web
WORKDIR /src/web
COPY web/package.json web/bun.lock ./
RUN bun install --frozen-lockfile
COPY web/ ./
RUN bun run build

FROM rust:1-bookworm AS build
WORKDIR /src
COPY Cargo.toml Cargo.lock ./
COPY crates crates
# The server embeds the built web app.
COPY --from=web /src/web/dist web/dist
RUN cargo build --release --locked -p spool

# 7-Zip from its authors: the build in Debian's main archive leaves out RAR support.
FROM debian:bookworm-slim AS sevenzip
ARG TARGETARCH
ARG SEVENZIP_VERSION=2409
RUN apt-get update && apt-get install -y --no-install-recommends ca-certificates curl xz-utils \
 && case "$TARGETARCH" in arm64) arch=arm64 ;; *) arch=x64 ;; esac \
 && curl -fsSL "https://www.7-zip.org/a/7z${SEVENZIP_VERSION}-linux-${arch}.tar.xz" | tar -xJ -C /usr/local/bin 7zz

FROM debian:bookworm-slim
RUN apt-get update \
 && apt-get install -y --no-install-recommends ca-certificates par2 ffmpeg tini \
 && rm -rf /var/lib/apt/lists/*
COPY --from=sevenzip /usr/local/bin/7zz /usr/local/bin/7zz
COPY --from=build /src/target/release/spool /usr/local/bin/spool

# Everything Spool keeps (database, backups, saved NZBs, artwork, log) lives in /data.
# HOME points there too so the container runs as any user without extra setup.
ENV SPOOL_DATA_DIR=/data HOME=/data SPOOL_BIND=0.0.0.0:7979
VOLUME /data
EXPOSE 7979
HEALTHCHECK --interval=30s --timeout=5s --start-period=20s CMD bash -c 'exec 3<>/dev/tcp/127.0.0.1/7979' || exit 1
ENTRYPOINT ["tini", "--", "spool"]
CMD ["serve"]
