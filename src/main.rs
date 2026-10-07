// Copyright 2020-2026 Eric B. Ridge <eebbrr@gmail.com>. All rights reserved. Use
// of this source code is governed by the Postgres license that can be found in
// the LICENSE file.
use crate::schema_set::SchemaSet;

mod nodes;
mod schema_set;

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let command = args.get(1).expect("no command argument");
    match command.as_str() {
        "deparse" => {
            let filename = args.get(2).expect("no filename argument");
            let mut set = SchemaSet::new();
            set.scan_file(filename);
            println!("{}", set.deparse());
        }
        "diff" => {
            let mut a = SchemaSet::new();
            let mut b = SchemaSet::new();
            a.scan_file(args.get(2).expect("no a filename"));
            b.scan_file(args.get(3).expect("no b filename"));
            println!("{}", a.diff(&b));
        }
        unknown => panic!("unrecognized command argument: {}", unknown),
    }
}
