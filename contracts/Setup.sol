// SPDX-License-Identifier: MIT
pragma solidity 0.8.28;
import "./IllusionHouse.sol";
import "./MirrorProxy.sol";
contract Setup {
    address public immutable VISITOR;
    address public immutable house;
    MirrorProxy public proxy;
    constructor(address visitor) payable {
        VISITOR = visitor;
        IllusionHouse implementation = new IllusionHouse();
        bytes memory initData =
            abi.encodeWithSignature("initialize(address)", address(this));
        proxy = new MirrorProxy{value: msg.value}( 
            address(implementation),
            initData,
            visitor
        );
        house = address(proxy);
    }
    function isSolved() external view returns (bool) {
        return IllusionHouse(house).roles(VISITOR) == IllusionHouse.Role.Curator;
    }
}