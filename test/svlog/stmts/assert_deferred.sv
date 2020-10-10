// RUN: moore %s -e foo

module foo;
    initial begin
        int x, y;
        assert #0 (x == y);
        assert final (x == y);
        assume #0 (x == y);
        assume final (x == y);
        cover #0 (x == y);
        cover final (x == y);
    end
endmodule
