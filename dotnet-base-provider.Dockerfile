FROM registry.access.redhat.com/ubi9/ubi

RUN dnf install -y dotnet-sdk-9.0

RUN dotnet tool install --global Paket
RUN dotnet tool install --global ilspycmd

