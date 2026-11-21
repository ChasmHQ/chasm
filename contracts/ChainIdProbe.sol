// SPDX-License-Identifier: MIT
pragma solidity ^0.8.20;

/// Reports the chain id the workspace node reports. A Local profile answers
/// 31337; a Source profile answers the forked chain's own id, because it runs
/// with that id.
contract ChainIdProbe {
    function chainId() external view returns (uint256) {
        return block.chainid;
    }
}
