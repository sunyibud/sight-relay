FROM rust:1.85-bookworm AS build
WORKDIR /src
COPY . .
RUN cargo build --release -p sight-relay-server
FROM debian:bookworm-slim
WORKDIR /app
COPY --from=build /src/target/release/sight-relay-server /usr/local/bin/sight-relay-server
ENV SIGHT_DATA_DIR=/data
VOLUME /data
EXPOSE 8080
CMD ["sight-relay-server"]
