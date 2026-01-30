// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;
// 0x8b85a99d4e556971bc8f3984389454ea791e7379
contract MirrorProxy {
    bytes32 private constant IMPLEMENTATION_SLOT =
        bytes32(uint256(keccak256("eip1967.proxy.implementation")) - 1);
    bytes32 private constant ADMIN_SLOT =
        bytes32(uint256(keccak256("eip1967.proxy.admin")) - 1);
    bytes32 private constant REFRAME_SLOT =
        bytes32(uint256(keccak256("illusion.proxy.reframed")) - 1);
    // The allowlist hash must match the approved runtime bytecode.
    // Any change that alters runtime bytecode will break the hash, including:
    // - Inheritance tree, base contracts, or linearization order.
    // - Function selectors, visibility, or dispatch logic.
    // - Storage layout (state variables, order, or types).
    // - Constants/immutables/constructor logic that affect runtime.
    // - Compiler settings (Solidity version, optimizer, or EVM target).
    bytes32 public constant ALLOWED_CODEHASH =
        0xf3138fd84a8d5cbde37c26366b84737db9968d3dc4a295948904ce513750b981;
    constructor(
        address implementation,
        bytes memory initData,
        address visitor
    ) payable {
        _setAdmin(visitor);
        _setImplementation(implementation);
        if (initData.length > 0) {
            (bool ok, ) = implementation.delegatecall(initData);
            require(ok, "init failed");
        }
    }
    function reframe(address newImplementation) external {
        require(!_getReframed(), "reframe used");
        require(
            _codeHash(newImplementation) == ALLOWED_CODEHASH,
            "invalid impl"
        );
        _setReframed(true);
        _setImplementation(newImplementation);
    }
    function reframed() external view returns (bool) {
        return _getReframed();
    }
    fallback() external payable {
        _delegate(_getImplementation());
    }
    receive() external payable {}
    function _getAdmin() internal view returns (address admin) {
        bytes32 slot = ADMIN_SLOT;
        assembly {
            admin := sload(slot)
        }
    }
    function _setAdmin(address admin) internal {
        bytes32 slot = ADMIN_SLOT;
        assembly {
            sstore(slot, admin)
        }
    }
    function _getImplementation() internal view returns (address implementation) {
        bytes32 slot = IMPLEMENTATION_SLOT;
        assembly {
            implementation := sload(slot)
        }
    }
    function _setImplementation(address implementation) internal {
        bytes32 slot = IMPLEMENTATION_SLOT;
        assembly {
            sstore(slot, implementation)
        }
    }
    function _getReframed() internal view returns (bool used) {
        bytes32 slot = REFRAME_SLOT;
        assembly {
            used := sload(slot)
        }
    }
    function _setReframed(bool used) internal {
        bytes32 slot = REFRAME_SLOT;
        assembly {
            sstore(slot, used)
        }
    }
    function _delegate(address implementation) internal {
        assembly {
            calldatacopy(0, 0, calldatasize())
            let result := delegatecall(
                gas(),
                implementation,
                0,
                calldatasize(),
                0,
                0
            )
            returndatacopy(0, 0, returndatasize())
            switch result
                case 0 {
                    revert(0, returndatasize())
                }
                default {
                    return(0, returndatasize())
                }
        }
    }
    function _codeHash(address target) internal view returns (bytes32 hash) {
        assembly {
            let size := extcodesize(target)
            switch lt(size, 2)
            case 1 { hash := 0 }
            default {
                let ptr := mload(0x40)
                extcodecopy(target, ptr, 0, size)
                // Last two bytes are CBOR length; strip metadata before hashing.
                let metaLen := shr(240, mload(add(ptr, sub(size, 2))))
                switch gt(add(metaLen, 2), size)
                case 1 { hash := 0 }
                default {
                    hash := keccak256(ptr, sub(size, add(metaLen, 2)))
                }
            }
        }
    }
}