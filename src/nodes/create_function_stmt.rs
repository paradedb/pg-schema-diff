// Copyright 2020-2026 Eric B. Ridge <eebbrr@gmail.com>. All rights reserved. Use
// of this source code is governed by the Postgres license that can be found in
// the LICENSE file.
use crate::schema_set::{
    function_identity, function_input_signature, Diff, Sql, SqlCollect, SqlIdent, SqlList,
};
use crate::{make_name, EMPTY_NODE_VEC};
use postgres_parser::nodes::CreateFunctionStmt;
use std::cmp::Ordering;

use postgres_parser::sys::FunctionParameterMode::FUNC_PARAM_TABLE;
use postgres_parser::Node;

impl Sql for CreateFunctionStmt {
    fn sql(&self) -> String {
        let mut returns_table = false;
        let mut sql = String::new();

        if self.replace {
            sql.push_str("CREATE OR REPLACE ");
        } else {
            sql.push_str("CREATE ");
        }

        if self.is_procedure {
            sql.push_str("PROCEDURE ");
        } else {
            sql.push_str("FUNCTION ");
        }

        sql.push_str(&self.funcname.sql_ident());
        sql.push_str(
            &self
                .parameters
                .as_ref()
                .unwrap_or(&EMPTY_NODE_VEC)
                .iter()
                .filter(|p| match p {
                    Node::FunctionParameter(param) if param.mode != FUNC_PARAM_TABLE => true,
                    Node::FunctionParameter(param) if param.mode == FUNC_PARAM_TABLE => {
                        returns_table = true;
                        false
                    }
                    _ => false,
                })
                .map(|node| node.clone())
                .sql_wrap("(", ")"),
        );

        if returns_table {
            sql.push_str(" RETURNS TABLE");

            sql.push_str(
                &self
                    .parameters
                    .as_ref()
                    .unwrap_or(&EMPTY_NODE_VEC)
                    .iter()
                    .filter(|p|
                        matches!(p, Node::FunctionParameter(param) if param.mode == FUNC_PARAM_TABLE)
                    )
                    .map(|node| node.clone())
                    .sql_wrap("(", ")"),
            );
        } else {
            sql.push_str(&self.returnType.sql_prefix(" RETURNS "));
        }

        let mut orig_options = self.options.clone();
        if let Some(mut options) = orig_options {
            options.sort_by(|a, b| match a {
                Node::DefElem(a_defelem) => {
                    if let Node::DefElem(b_defelem) = b {
                        return a_defelem.sql().cmp(&b_defelem.sql());
                    }

                    Ordering::Equal
                }

                _ => Ordering::Equal,
            });
            orig_options = Some(options);
        }

        sql.push_str(&orig_options.sql_prefix(" ", " "));

        sql
    }
}

impl Diff for CreateFunctionStmt {
    fn alter_stmt(&self, other: &Node) -> Option<String> {
        if let Node::CreateFunctionStmt(other_stmt) = other {
            // CREATE OR REPLACE preserves the function's OID so dependent
            // objects (views, triggers, other functions) survive. Postgres
            // forbids it when the return type — or RETURNS TABLE column
            // types — change, so fall back to drop+create in that case.
            if return_signature(self) == return_signature(other_stmt) {
                let mut replaced = other_stmt.clone();
                replaced.replace = true;
                return Some(replaced.sql());
            }
        }
        let mut alter = String::new();
        alter.push_str(&self.drop_stmt().unwrap());
        alter.push_str(";\n");
        alter.push_str(&other.sql());
        Some(alter)
    }

    fn drop_stmt(&self) -> Option<String> {
        let mut drop = String::new();

        drop.push_str("DROP ");
        if self.is_procedure {
            drop.push_str("PROCEDURE ");
        } else {
            drop.push_str("FUNCTION ");
        }
        drop.push_str("IF EXISTS ");
        drop.push_str(&make_name(&self.funcname).expect("no 'funcname' for CreateFunctionStmt"));
        drop.push('(');
        drop.push_str(
            &self
                .parameters
                .as_ref()
                .unwrap_or(&EMPTY_NODE_VEC)
                .iter()
                .filter(|p| {
                    matches!(p,
                    Node::FunctionParameter(param) if param.mode != FUNC_PARAM_TABLE)
                })
                .map(|node| match node {
                    Node::FunctionParameter(fp) => {
                        let mut fp = fp.clone();
                        fp.defexpr = None;
                        Node::FunctionParameter(fp)
                    }
                    _ => panic!("unexpected function parameter node type"),
                })
                .sql(),
        );
        drop.push(')');
        Some(drop)
    }

    fn object_name(&self) -> Option<String> {
        let name =
            make_name(&self.funcname).expect("unable to make name for CreateFunctionStatement");
        Some(name + &function_input_signature(self.parameters.as_ref().unwrap_or(&EMPTY_NODE_VEC)))
    }

    fn object_type(&self) -> String {
        "FUNCTION".into()
    }

    fn schema_object_identities(&self) -> Vec<String> {
        let kind = if self.is_procedure {
            "PROCEDURE"
        } else {
            "FUNCTION"
        };
        let name =
            make_name(&self.funcname).expect("unable to make name for CreateFunctionStatement");
        vec![function_identity(
            kind,
            &name,
            self.parameters.as_ref().unwrap_or(&EMPTY_NODE_VEC),
        )]
    }
}

fn return_signature(stmt: &CreateFunctionStmt) -> String {
    let table_cols = stmt
        .parameters
        .as_ref()
        .unwrap_or(&EMPTY_NODE_VEC)
        .iter()
        .filter(|p| matches!(p, Node::FunctionParameter(param) if param.mode == FUNC_PARAM_TABLE))
        .map(|node| match node {
            Node::FunctionParameter(fp) => {
                let mut fp = fp.clone();
                fp.defexpr = None;
                Node::FunctionParameter(fp)
            }
            _ => unreachable!(),
        })
        .sql_wrap("(", ")");
    format!("{}|{}", stmt.returnType.sql(), table_cols)
}
