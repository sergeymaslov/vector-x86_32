#! /usr/bin/env bash
set -e -o verbose

rustup default "$(cat rust-toolchain)"
rustup component add rustfmt
rustup component add clippy
rustup target add wasm32-wasi

cd scripts
bundle update --bundler
bundle install
cd ..

cd website
yarn install
cd ..
