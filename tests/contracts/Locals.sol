// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

/// Every shape of local variable that breaks a naive stack-slot reconstruction.
contract Locals {
    struct Point {
        uint256 x;
        uint256 y;
    }

    uint256 public counter;
    uint256[] public numbers;
    mapping(address => Point) public points;

    /// Value types of several widths, plus one that is only assigned later.
    function values(uint256 seed) public pure returns (uint256) {
        uint256 doubled = seed * 2;
        int128 negative = -7;
        bool flag = doubled > 10;
        address who = address(0x1234);
        bytes4 tag = 0xdeadbeef;
        uint8 small = 200;
        return doubled + uint256(int256(negative) + 8) + (flag ? 1 : 0) + small + uint160(who) + uint32(tag);
    }

    /// A block that ends. `shadowed` must not resurface after the `if` closes, and the
    /// outer `x` must be the one visible on the last line.
    function scoping(uint256 x) public pure returns (uint256) {
        uint256 total = x;
        if (x > 0) {
            uint256 shadowed = 111;
            total += shadowed;
        }
        {
            uint256 inner = 222;
            total += inner;
        }
        uint256 after_ = 333;
        return total + after_;
    }

    /// Two live frames of the same function, each with its own base.
    function recurse(uint256 depth) public pure returns (uint256) {
        uint256 here = depth * 10;
        if (depth == 0) {
            return here;
        }
        uint256 below = recurse(depth - 1);
        return here + below;
    }

    /// A loop variable is re-allocated every time round.
    function loop(uint256 rounds) public pure returns (uint256 sum) {
        for (uint256 i = 0; i < rounds; i++) {
            uint256 square = i * i;
            sum += square;
        }
    }

    modifier counted() {
        uint256 before = counter;
        _;
        require(counter >= before, "counter went backwards");
    }

    /// Locals declared in a modifier body live in the same EVM frame as the function's.
    function bump(uint256 by) public counted returns (uint256) {
        uint256 next = counter + by;
        counter = next;
        return next;
    }

    /// Memory reference types: a pointer on the stack, the value behind it.
    function memoryTypes(string memory label) public pure returns (uint256) {
        uint256[] memory list = new uint256[](3);
        list[0] = 10;
        list[1] = 20;
        list[2] = 30;
        bytes memory raw = bytes(label);
        uint256 len = list.length + raw.length;
        return len;
    }

    /// A storage pointer holds a slot number, not a value.
    function storagePointer(uint256 value) public returns (uint256) {
        Point storage p = points[msg.sender];
        p.x = value;
        uint256 read = p.x;
        return read;
    }

    /// Calldata references are two stack slots, offset and length.
    function calldataTypes(bytes calldata payload) public pure returns (uint256) {
        uint256 size = payload.length;
        return size;
    }

    function fill(uint256 count) public {
        for (uint256 i = 0; i < count; i++) {
            numbers.push(i);
        }
    }
}
