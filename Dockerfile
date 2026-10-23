# syntax=docker/dockerfile:1.7
# RustCraft: the site (launcher, rjava.wasm) and its server (static files with
# COOP/COEP headers + the CORS proxy, launcher/proxy/server.ts), built from
# source:
#   docker build -t rustcraft .
#   docker run -p 8080:8080 rustcraft          # http://localhost:8080/
# The first build compiles shaderc, SPIRV-Tools, glslang and SPIRV-Cross for
# wasm (tens of minutes); later builds reuse that layer. With an rjava.wasm
# built locally, Dockerfile.prebuilt is much faster.
# The build stages run on x86-64 (the wasi-sdk binaries); the result is the
# same on any platform.

# --- C/C++ libraries for wasm (vm/native-libs) --------------------------------
FROM --platform=linux/amd64 debian:trixie-slim AS wasm-libs
RUN apt-get update && apt-get install -y --no-install-recommends \
      ca-certificates curl git cmake ninja-build python3 patch \
    && rm -rf /var/lib/apt/lists/*
COPY vm/native-libs /src/native-libs
RUN /src/native-libs/fetch.sh /wasm-libs && /wasm-libs/build.sh \
    && rm -rf /wasm-libs/build /wasm-libs/src /wasm-libs/tmp

# --- the VM: rjava.wasm ---------------------------------------------------------
FROM --platform=linux/amd64 rust:1.99-slim AS vm
RUN rustup target add wasm32-wasip1-threads
COPY --from=wasm-libs /wasm-libs /wasm-libs
ENV RJAVA_WASM_LIBS=/wasm-libs
COPY vm /src/vm
WORKDIR /src/vm
RUN --mount=type=cache,target=/usr/local/cargo/registry \
    cargo build --release --locked -p rjava --target wasm32-wasip1-threads \
    && cp target/wasm32-wasip1-threads/release/rjava.wasm /rjava.wasm

# --- the site: launcher/dist ----------------------------------------------------
FROM node:24-slim AS site
RUN corepack enable && corepack prepare pnpm@11.28.4 --activate
WORKDIR /src/launcher
COPY launcher/package.json launcher/pnpm-lock.yaml ./
RUN pnpm install --frozen-lockfile
COPY launcher ./
# vite.config.ts takes rjava.wasm from the VM's build directory.
COPY --from=vm /rjava.wasm /src/vm/target/wasm32-wasip1-threads/release/rjava.wasm
# Optional: another Microsoft application (Azure) client ID for sign-in.
ARG VITE_MS_CLIENT_ID=
ENV VITE_MS_CLIENT_ID=$VITE_MS_CLIENT_ID
RUN pnpm build

# --- runtime ----------------------------------------------------------------------
FROM node:24-slim
WORKDIR /app
COPY --from=site /src/launcher/dist ./dist
COPY launcher/proxy/server.ts launcher/proxy/proxy.ts ./proxy/
COPY LICENSE ./
# Behind a reverse proxy, ingress or Cloudflare (HTTPS terminated there) set
# TRUST_PROXY=1 so rate limits use the client's address; ALLOWED_ORIGINS
# (comma-separated) if the site is served under another origin than
# https://rustcraft.online; TLS_CERT/TLS_KEY (file paths) to serve HTTPS here.
ENV NODE_ENV=production STATIC_DIR=/app/dist HOST=0.0.0.0 PORT=8080
USER node
EXPOSE 8080
HEALTHCHECK --interval=30s --timeout=5s CMD ["node", "-e", "fetch('http://127.0.0.1:'+process.env.PORT+'/').then(r=>process.exit(r.ok?0:1),()=>process.exit(1))"]
CMD ["node", "proxy/server.ts"]
