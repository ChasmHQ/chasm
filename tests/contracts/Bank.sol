// SPDX-License-Identifier: MIT
pragma solidity ^0.8.0;

/// Exercises the debugger against every feature that is hard to get right:
/// packed slots, mappings, dynamic arrays, strings, structs, internal calls,
/// modifiers, loops, external calls into another contract, and reverts.
contract Bank {
    struct Account {
        uint128 balance;   // packed with `frozen` in one slot
        bool frozen;
        string nickname;
    }

    address public owner;              // slot 0
    uint96 public feeBps;              // slot 0, packed after owner
    uint256 public totalDeposits;      // slot 1
    mapping(address => uint256) public balances;   // slot 2
    mapping(address => Account) public accounts;   // slot 3
    uint256[] public history;          // slot 4
    string public name;                // slot 5

    event Deposited(address indexed who, uint256 amount);

    error NotOwner(address caller);

    modifier onlyOwner() {
        if (msg.sender != owner) revert NotOwner(msg.sender);
        _;
    }

    constructor(string memory _name) payable {
        owner = msg.sender;
        feeBps = 25;
        name = _name;
        balances[msg.sender] = msg.value;
        totalDeposits = msg.value;
    }

    /// Internal call, so it compiles to a JUMP and the EVM depth never changes.
    function _fee(uint256 amount) internal view returns (uint256) {
        return (amount * feeBps) / 10000;
    }

    function _credit(address who, uint256 amount) internal {
        uint256 fee = _fee(amount);
        balances[who] += amount - fee;
        totalDeposits += amount - fee;
        history.push(amount);
    }

    function deposit() public payable {
        _credit(msg.sender, msg.value);
        emit Deposited(msg.sender, msg.value);
    }

    function setNickname(string memory nick) public {
        accounts[msg.sender].nickname = nick;
        accounts[msg.sender].balance = uint128(balances[msg.sender]);
    }

    function freeze(address who) public onlyOwner {
        accounts[who].frozen = true;
    }

    /// A loop, so `next` has something to iterate over.
    function sumHistory() public view returns (uint256 total) {
        for (uint256 i = 0; i < history.length; i++) {
            total += history[i];
        }
    }

    function withdraw(uint256 amount) public {
        require(balances[msg.sender] >= amount, "insufficient balance");
        balances[msg.sender] -= amount;
        totalDeposits -= amount;
    }

    /// Real external CALL, so a new EVM frame appears and `bt` has two levels.
    function forward(address target, uint256 value) public returns (uint256) {
        return Callee(target).receiveValue(value);
    }

    function boom() public pure {
        revert("kaboom");
    }

    function overflow(uint256 a, uint256 b) public pure returns (uint256) {
        return a - b;   // panics 0x11 when b > a
    }
}

contract Callee {
    uint256 public last;

    function receiveValue(uint256 v) public returns (uint256) {
        last = v;
        return v * 2;
    }
}
