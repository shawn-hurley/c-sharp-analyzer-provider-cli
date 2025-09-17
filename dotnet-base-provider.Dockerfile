FROM registry.access.redhat.com/ubi9/ubi as builder

RUN dnf install -y rust-toolset unzip

RUN curl -LO https://github.com/protocolbuffers/protobuf/releases/download/v30.2/protoc-30.2-linux-x86_64.zip &&\
    unzip protoc-30.2-linux-x86_64.zip -d $HOME/protoc

WORKDIR /csharp-provider
COPY . /csharp-provider/

RUN PROTOC=$HOME/protoc/bin/protoc cargo build

FROM registry.access.redhat.com/ubi9/ubi

RUN dnf install -y dotnet-sdk-9.0

RUN dotnet tool install --global Paket
RUN dotnet tool install --global ilspycmd

COPY --from=builder /csharp-provider/target/debug/c-sharp-analyzer-provider-cli /usr/local/bin/c-sharp-provider
ENTRYPOINT ["/usr/local/bin/c-sharp-provider", "--port", "9000", "--name", "c-sharp"]
