#!/bin/sh
RUST_LOG=c_sharp_analyzer_provider_cli=DEBUG,INFO target/debug/c-sharp-analyzer-provider-cli --port 9000 --name c-sharp --db-path demo.db &> demo.log & export SERVER_PID=$!;
echo ${SERVER_PID};
