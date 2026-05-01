// Copyright 2020-2026 Eric B. Ridge <eebbrr@gmail.com>. All rights reserved. Use
// of this source code is governed by the Postgres license that can be found in
// the LICENSE file.
use crate::schema_set::{
    cast_identity, function_identity, operator_identity, simple_identity, Diff, Sql, SqlIdent,
};
use crate::EMPTY_NODE_VEC;
use postgres_parser::nodes::DropStmt;
use postgres_parser::sys::ObjectType;
use postgres_parser::Node;

impl Sql for DropStmt {
    fn sql(&self) -> String {
        let mut sql = String::new();

        sql.push_str("DROP ");
        sql.push_str(&self.removeType.sql());
        sql.push(' ');
        if self.concurrent {
            sql.push_str("CONCURRENTLY ");
        }
        if self.missing_ok {
            sql.push_str("IF EXISTS ");
        }

        match self.removeType {
            ObjectType::OBJECT_RULE => {
                let objects = self.objects.as_ref().unwrap();
                if let Node::List(objects) = objects.get(0).as_ref().unwrap() {
                    let tablename = objects.get(0).unwrap();
                    let rulename = objects.get(1).unwrap();
                    sql.push_str(&rulename.sql_ident());
                    sql.push_str(" ON ");
                    sql.push_str(&tablename.sql_ident());
                }
            }
            _ => {
                for (i, node) in self.objects.as_ref().unwrap().iter().enumerate() {
                    if i > 0 {
                        sql.push_str(", ");
                    }
                    if let Node::List(names) = node {
                        sql.push_str(&names.sql_ident());
                    } else if let Node::Value(_) = node {
                        sql.push_str(&node.sql_ident());
                    } else {
                        sql.push_str(&node.sql());
                    }
                }
            }
        }

        sql.push_str(&self.behavior.sql_prefix(" "));

        sql
    }
}

impl Diff for DropStmt {
    fn schema_object_identities(&self) -> Vec<String> {
        let mut result = Vec::new();
        let Some(objects) = self.objects.as_ref() else {
            return result;
        };
        let kind = self.removeType.sql();
        for obj in objects {
            let id = match self.removeType {
                ObjectType::OBJECT_FUNCTION | ObjectType::OBJECT_PROCEDURE => {
                    let Node::ObjectWithArgs(owa) = obj else {
                        continue;
                    };
                    let name = owa.objname.sql_ident();
                    let parameters = if owa.args_unspecified {
                        &EMPTY_NODE_VEC
                    } else {
                        owa.objargs.as_ref().unwrap_or(&EMPTY_NODE_VEC)
                    };
                    function_identity(&kind, &name, parameters)
                }
                ObjectType::OBJECT_OPERATOR => {
                    let Node::ObjectWithArgs(owa) = obj else {
                        continue;
                    };
                    let args = owa.objargs.as_ref();
                    let (leftarg, rightarg) = args
                        .and_then(|a| a.get(0..=1))
                        .map(|nodes| (nodes[0].sql(), nodes[1].sql()))
                        .unwrap_or_default();
                    operator_identity(&owa.objname.sql_ident(), &leftarg, &rightarg)
                }
                ObjectType::OBJECT_AGGREGATE => {
                    // Mirrors DefineStmt's catch-all (no args) — see the
                    // limitation note on `DefineStmt::schema_object_identities`.
                    let Node::ObjectWithArgs(owa) = obj else {
                        continue;
                    };
                    simple_identity(&kind, &owa.objname.sql_ident())
                }
                ObjectType::OBJECT_CAST => {
                    let Node::List(parts) = obj else { continue };
                    if parts.len() != 2 {
                        continue;
                    }
                    cast_identity(&parts[0].sql(), &parts[1].sql())
                }
                ObjectType::OBJECT_SCHEMA | ObjectType::OBJECT_TYPE | ObjectType::OBJECT_VIEW => {
                    let name = match obj {
                        Node::List(names) => names.sql_ident(),
                        Node::Value(_) => obj.sql_ident(),
                        // e.g. Node::TypeName for `DROP TYPE color` — mirrors
                        // the fallback in `Sql for DropStmt`.
                        other => other.sql(),
                    };
                    simple_identity(&kind, &name)
                }
                _ => continue,
            };
            result.push(id);
        }
        result
    }
}
