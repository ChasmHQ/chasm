// SPDX-License-Identifier: MIT
pragma solidity ^0.8.28;

contract WorkshopVault {
    address public owner;
    uint96 public feeBps;
    mapping(address => uint256) public balances;
    uint256 public total;

    event Withdrawal(address indexed account, uint256 amount, uint256 fee);

    function _fee(uint256 amount) internal view returns (uint256) {
        return amount * feeBps / 10000;
    }

    function withdraw(uint256 amount) external {
        require(balances[msg.sender] >= amount, "insufficient balance");
        uint256 fee = _fee(amount);
        balances[msg.sender] -= amount;
        total -= amount - fee;
        emit Withdrawal(msg.sender, amount, fee);
    }
}
