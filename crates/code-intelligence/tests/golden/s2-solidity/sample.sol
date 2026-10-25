pragma solidity ^0.8.0;

import "./Other.sol";

contract Graph {
    function add(uint256 a) public pure returns (uint256) {
        return check(a);
    }

    function check(uint256 a) internal pure returns (uint256) {
        return a + 1;
    }
}
