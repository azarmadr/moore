// RUN: moore %s -e foo

module foo;
    logic [7:0] a;
    logic [3:0] b;
    logic [11:0] c;
    logic [1:0][3:0] d;
    initial begin
        {a} = 8'd42;
        // CHECK: %1 = const i8 42
        // CHECK: drv i8$ %a, %1, %2
        {a, b} = 12'd42;
        // CHECK: %3 = const i4 10
        // CHECK: drv i4$ %b, %3, %2
        // CHECK: %4 = const i8 2
        // CHECK: drv i8$ %a, %4, %2
        {a, b} = c;
        // CHECK: %c.prb = prb i12$ %c
        // CHECK: %5 = exts i4, i12 %c.prb, 0, 4
        // CHECK: drv i4$ %b, %5, %2
        // CHECK: %6 = exts i8, i12 %c.prb, 4, 8
        // CHECK: drv i8$ %a, %9, %2
        {a[1:0], b[1:0]} = 4'hA;
        // CHECK: %10 = exts i2$, i4$ %b, 0, 2
        // CHECK: %11 = const i2 2
        // CHECK: drv i2$ %10, %11, %2
        // CHECK: %12 = exts i2$, i8$ %a, 0, 2
        // CHECK: drv i2$ %12, %11, %2
        // {d} = 8'd42; // BROKEN until #208 lands
    end
endmodule
