#!/bin/bash

# This script is run after Renovate upgrades dependencies or lock files.

set -euo pipefail

# Function to retry a command up to 3 times.
function retry_command {
    local retries=3
    local delay=5
    local count=0
    until "$@"; do
        exit_code=$?
        count=$((count+1))
        if [ $count -lt $retries ]; then
            echo "Command failed with exit code $exit_code. Retrying in $delay seconds..."
            sleep $delay
        else
            echo "Command failed with exit code $exit_code after $count attempts."
            return $exit_code
        fi
    done
}

# Download and install cargo-hakari if it is not already installed.
if ! command -v cargo-hakari &> /dev/null; then
    # Need cargo-binstall to install cargo-hakari.
    if ! command -v cargo-binstall &> /dev/null; then
        # Fetch cargo binstall.
        echo "Installing cargo-binstall..."
        curl --retry 3 -L --proto '=https' --tlsv1.2 -sSfO https://raw.githubusercontent.com/cargo-bins/cargo-binstall/main/install-from-binstall-release.sh
        retry_command bash install-from-binstall-release.sh
    fi

    # Install cargo-hakari.
    echo "Installing cargo-hakari..."
    retry_command cargo binstall cargo-hakari --no-confirm
fi

# Run cargo hakari to regenerate the workspace-hack file.
echo "Running cargo-hakari..."
cargo hakari generate