// SPDX-License-Identifier: MIT
pragma solidity >=0.6.2 <0.9.0;

// A library's own tests must never reach solc through sevm: this one compiles to an
// unlinked `__$...$__` placeholder, which has no debuggable artifact. If the import
// closure ever widens to a library's test/ tree, the suite notices here.
library Unlinkable {
    function double(uint256 x) external pure returns (uint256) {
        return x * 2;
    }
}

contract UsesUnlinkable {
    function run(uint256 x) external pure returns (uint256) {
        return Unlinkable.double(x);
    }
}
