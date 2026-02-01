#!/bin/bash

echo "building frontend..."
npm --prefix ui run build

echo "building binary..."
cargo build --release
