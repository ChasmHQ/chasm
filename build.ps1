Write-Output "building frontend..."
npm --prefix ui run build

Write-Output "building binary..."
cargo build --release