FROM ubuntu:22.04
COPY ./target/release/my-no-sql-server-grpc ./target/release/my-no-sql-server-grpc
COPY ./wwwroot ./wwwroot
ENTRYPOINT ["./target/release/my-no-sql-server-grpc"]