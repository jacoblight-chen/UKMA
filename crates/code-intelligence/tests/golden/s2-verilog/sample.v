module demo (
    input clk
);
  function integer compute;
    input integer size;
    compute = size + 1;
  endfunction
endmodule
