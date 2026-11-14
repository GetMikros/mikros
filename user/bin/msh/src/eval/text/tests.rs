use crate::eval::tests::{ok, run, shell};

#[test]
fn splits_text() {
    assert_eq!(ok("echo \"a b\\nc\" | lines | len | print"), "2\n");
    assert_eq!(ok("echo ' a  b ' | words | print"), "a\nb\n");
    assert_eq!(
        ok("echo 'a,b,,c' | trim | split ',' | print"),
        "a\nb\n\nc\n"
    );
    assert_eq!(ok("echo '  x ' | trim | upper | print"), "X\n");
}

#[test]
fn reads_tables_and_patterns() {
    let table = "echo \"NAME   SIZE  KIND\\nann    10    file\\nbob    2     dir\"";
    assert_eq!(
        ok(&format!("{table} | table | get NAME | print")),
        "ann\nbob\n"
    );
    assert_eq!(
        ok(&format!(
            "{table} | table | where {{ ($it.KIND == 'dir') }} | get SIZE | print"
        )),
        "2\n"
    );
    assert_eq!(
        ok(
            "echo \"ann=1\\nbob=22\\nnot this\" | parse '{name}={n}' | each { ($it.name + $it.n) } | print"
        ),
        "ann1\nbob22\n"
    );
    assert_eq!(
        ok("echo \"id 7\\nno\\nid 12\" | match 'id (?P<n>\\d+)' | get n | print"),
        "7\n12\n"
    );
    assert_eq!(
        ok("echo 'a1b22' | trim | replace '\\d+' '#' | print"),
        "a#b#\n"
    );
}

#[test]
fn works_on_lists_and_records() {
    assert_eq!(ok("([3 1 2]) | sort-by | print"), "1\n2\n3\n");
    assert_eq!(
        ok("([{n: 2} {n: 1}]) | sort-by n | get n | print"),
        "1\n2\n"
    );
    assert_eq!(
        ok("([1 2 3 4]) | where { ($it % 2 == 0) } | print"),
        "2\n4\n"
    );
    assert_eq!(
        ok("([1 2 3]) | each { ($it * 10) } | print"),
        "10\n20\n30\n"
    );
    assert_eq!(ok("([1 1 2 1]) | uniq | print"), "1\n2\n");
    assert_eq!(ok("([1 2 3]) | first | print"), "1\n");
    assert_eq!(ok("([1 2 3]) | first 2 | print"), "1\n2\n");
    assert_eq!(ok("([1 2 3]) | skip 2 | print"), "3\n");
    assert_eq!(ok("([a b]) | contains b | print"), "true\n");
    assert_eq!(
        ok("([{k: 'x', v: 1} {k: 'y', v: 2} {k: 'x', v: 3}]) | group-by k | get x | get v | print"),
        "1\n3\n"
    );
    // Blocks read and change the variables around them.
    assert_eq!(
        ok("let total = 0\n([1 2 3]) | each { total = $total + $it }\nprint $total"),
        "6\n"
    );
}

#[test]
fn reads_and_writes_json() {
    assert_eq!(
        ok("echo '{\"a\": [1, 2.5, null], \"b\": \"x\"}' | from json | get a.1 | print"),
        "2.5\n"
    );
    assert_eq!(
        ok("({a: [1 true]}) | to json | print"),
        "{\"a\":[1,true]}\n"
    );
    let mut s = shell();
    assert!(run(&mut s, "echo '{' | from json").is_err());
}

#[test]
fn takes_blocks_whose_statements_are_expressions() {
    assert_eq!(ok("[1 2 3] | each { $it + 1 } | print"), "2\n3\n4\n");
    assert_eq!(ok("[1 2 3] | where { $it > 1 } | print"), "2\n3\n");
}

#[test]
fn takes_a_single_record_as_a_list_that_holds_it() {
    let r = "let r = {name: 'a', size: 2mb}\n";
    let out = |source: &str| ok(&format!("{r}{source}"));
    assert_eq!(out("$r | where { $it.size > 1mb } | len | print"), "1\n");
    assert_eq!(out("$r | where { $it.size > 9mb } | len | print"), "0\n");
    assert_eq!(out("$r | each { $it.name } | print"), "a\n");
    assert_eq!(out("$r | first | get name | print"), "a\n");
    assert_eq!(out("$r | first 5 | len | print"), "1\n");
    assert_eq!(out("$r | skip | len | print"), "0\n");
    assert_eq!(out("$r | uniq | len | print"), "1\n");
    assert_eq!(out("$r | sort-by size | len | print"), "1\n");
    assert_eq!(out("$r | group-by name | get a | len | print"), "1\n");
    // `get` and `len` still take the record itself.
    assert_eq!(out("$r | get name | print"), "a\n");
    assert_eq!(out("$r | len | print"), "2\n");
}
