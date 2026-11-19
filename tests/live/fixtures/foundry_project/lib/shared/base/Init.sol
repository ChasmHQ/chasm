// SPDX-License-Identifier: MIT
pragma solidity ^0.8.13;

abstract contract Init {
    bool internal ready;

    modifier initializer() {
        require(!ready, "already initialized");
        ready = true;
        _;
    }
}
