entity foo is end;
architecture bar of foo is

	type BIT is ('0', '1');
	signal A : BIT;

	function F1 return BIT is
		--variable X : BIT;
	begin
		wait;
		wait on A;
		wait for blah;
		wait until false;
		wait for 10 ns;

		assert false;
		assert false report "holy moly";
		assert false severity warning;
		assert false report "explosion" severity error;

		report "hello";
		report "hello" severity warning;

		X := '0';
		X := '0' when true else '1';
		with 123 select X :=
			'0' when 1,
			'0' when 2|3,
			'0' when 4 to 10,
			'0' when asdf,
			'1' when others;

		--F1(x);
		--Image(x);

		if true then
			wait;
		elsif false then
			wait;
		else
			wait;
		end if;

		case 123 is
			when 1 => wait;
			when 2|3 => wait;
			when 4 to 10 => wait;
			when asdf => wait;
			when others => wait;
		end case;

		while true loop
			wait;
		end loop;

		for x in 0 to 31 loop
			wait;
		end loop;

		l0: for x in 0 to 31 loop
			next;
			next when false;
			--next l0;
			--next l0 when false;
		end loop;

		l1: for x in 0 to 31 loop
			exit;
			exit when true;
			--exit l1;
			--exit l1 when true;
		end loop;

		return;
		return 1234;

		null;
	end;

begin end;
