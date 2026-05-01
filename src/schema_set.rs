// Copyright 2020-2026 Eric B. Ridge <eebbrr@gmail.com>. All rights reserved. Use
// of this source code is governed by the Postgres license that can be found in
// the LICENSE file.
use crate::{make_name, EMPTY_NODE_VEC};
use postgres_parser::{parse_query, quote_identifier, Node, SqlStatementScanner};

use colored::Colorize;
use std::borrow::Cow;

use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::panic::{catch_unwind, RefUnwindSafe};

#[derive(Debug)]
pub struct DiffableStatement {
    tree_string: String,
    sql: String,
    node: Node,
    differ: Box<dyn Diff>,
}

impl Hash for DiffableStatement {
    fn hash<H: Hasher>(&self, state: &mut H) {
        self.differ.identifier(&self.tree_string).hash(state)
    }
}

impl Eq for DiffableStatement {}
impl PartialEq for DiffableStatement {
    fn eq(&self, other: &Self) -> bool {
        self.differ
            .identifier(&self.tree_string)
            .eq(&other.differ.identifier(&other.tree_string))
    }
}

impl RefUnwindSafe for DiffableStatement {}

impl DiffableStatement {
    fn new(sql: &str, node: Node, differ: impl Diff + 'static) -> Self {
        let mut sql = sql.trim();
        if sql.ends_with(';') {
            sql = &sql[..sql.len() - 1];
        }
        DiffableStatement {
            tree_string: format!("{:?}", node),
            sql: sql.trim().into(),
            node,
            differ: Box::new(differ),
        }
    }
}

pub trait Diff: Sql + Debug {
    fn alter_stmt(&self, _other: &Node) -> Option<String> {
        println!(
            "/*\nDon't know how to ALTER:\n{}\n{:?}\n{:?}\n*/",
            self.sql(),
            _other,
            self
        );
        return None;
    }

    fn drop_stmt(&self) -> Option<String> {
        unimplemented!("Don't know how to drop: {:#?}", self)
    }

    /// Canonical identity strings for the schema object(s) this statement
    /// targets, used by the `validate` command to match expected drops/creates
    /// against the upgrade script. Both the create-side statements and
    /// `DropStmt` should produce identical strings for the same logical
    /// object — see the `*_identity` helpers below for the canonical formats.
    fn schema_object_identities(&self) -> Vec<String> {
        Vec::new()
    }

    fn object_name(&self) -> Option<String> {
        None
    }

    fn object_type(&self) -> String {
        String::new()
    }

    fn identifier<'a>(&self, tree_string: &'a str) -> Cow<'a, str> {
        match self.object_name() {
            Some(name) => Cow::Owned(name + &self.object_type()),
            None => Cow::Borrowed(tree_string),
        }
    }
}

// Identity-builder helpers shared by the create-side `Diff` impls and
// `DropStmt`. Both sides MUST go through these so the strings stay in sync
// if the format ever changes.
pub fn function_identity<'a>(
    kind: &str,
    name: &str,
    parameters: impl IntoIterator<Item = &'a Node>,
) -> String {
    format!("{}:{}{}", kind, name, function_input_signature(parameters))
}

// Type-only input parameter list, used as the function's identity.
// Matches Postgres overload semantics: function identity is name + input
// argument types. Parameter names and defaults are stripped so they don't
// participate in the match.
pub fn function_input_signature<'a>(parameters: impl IntoIterator<Item = &'a Node>) -> String {
    use postgres_parser::sys::FunctionParameterMode::FUNC_PARAM_TABLE;

    parameters
        .into_iter()
        .filter_map(|node| match node {
            Node::FunctionParameter(fp) if fp.mode == FUNC_PARAM_TABLE => None,
            Node::FunctionParameter(fp) => {
                let mut fp = fp.clone();
                fp.name = None;
                fp.defexpr = None;
                Some(Node::FunctionParameter(fp))
            }
            // Bare type nodes (e.g. from DROP FUNCTION foo(int, text)) — pass
            // through; their `.sql()` already produces just the type string.
            other => Some(other.clone()),
        })
        .sql_wrap("(", ")")
}

pub fn cast_identity(source: &str, target: &str) -> String {
    format!("CAST:({} AS {})", source, target)
}

pub fn operator_identity(name: &str, leftarg: &str, rightarg: &str) -> String {
    format!("OPERATOR:{}({}, {})", name, leftarg, rightarg)
}

pub fn simple_identity(kind: &str, name: &str) -> String {
    format!("{}:{}", kind, name)
}

pub trait SqlMaybeList {
    fn sql_maybe_list(&self, sep: &str) -> String;
}

impl SqlMaybeList for Option<Box<Node>> {
    fn sql_maybe_list(&self, sep: &str) -> String {
        match self {
            None => String::new(),
            Some(boxed_sql) => boxed_sql.sql_maybe_list(sep),
        }
    }
}

impl SqlMaybeList for Node {
    fn sql_maybe_list(&self, sep: &str) -> String {
        if let Node::List(v) = self {
            v.sql(sep)
        } else {
            self.sql()
        }
    }
}

pub trait Sql {
    fn sql_prefix(&self, pre: &str) -> String {
        format!("{}{}", pre, self.sql())
    }
    #[track_caller]
    fn sql_wrap(&self, pre: &str, post: &str) -> String {
        format!("{}{}{}", pre, self.sql(), post)
    }
    fn sql_prefix_and_wrap(&self, pre: &str, start: &str, end: &str) -> String {
        format!("{}{}{}{}", pre, start, self.sql(), end)
    }
    fn sql(&self) -> String;
}

pub trait SqlList {
    fn sql(&self, sep: &str) -> String;
    fn sql_prefix(&self, pre: &str, sep: &str) -> String;
    fn sql_prefix_and_wrap(&self, pre: &str, start: &str, end: &str, sep: &str) -> String;
    fn sql_wrap_each(&self, pre: Option<&str>, post: Option<&str>) -> String;
    fn sql_wrap_each_and_separate(&self, sep: &str, pre: &str, post: &str) -> String;
    fn sql_wrap(&self, sep: &str, pre: &str, post: &str) -> String;
}

pub trait SqlIdent {
    fn sql_ident(&self) -> String;
    fn sql_ident_prefix(&self, pre: &str) -> String;
    fn sql_ident_suffix(&self, suf: &str) -> String;
}

pub trait SqlCollect {
    fn sql_wrap(self, pre: &str, post: &str) -> String;
    fn sql(self) -> String;
}

impl<T: Sql> Sql for Option<Box<T>> {
    fn sql_prefix(&self, pre: &str) -> String {
        match self {
            None => String::new(),
            Some(boxed_sql) => format!("{}{}", pre, boxed_sql.sql()),
        }
    }

    fn sql_wrap(&self, pre: &str, post: &str) -> String {
        match self {
            None => String::new(),
            Some(boxed_sql) => boxed_sql.sql_wrap(pre, post),
        }
    }

    fn sql_prefix_and_wrap(&self, pre: &str, start: &str, end: &str) -> String {
        match self {
            None => String::new(),
            Some(boxed_sql) => format!("{}{}{}{}", pre, start, boxed_sql.sql(), end),
        }
    }

    #[track_caller]
    fn sql(&self) -> String {
        match self {
            None => String::new(),
            Some(boxed_sql) => boxed_sql.sql(),
        }
    }
}

impl SqlIdent for Option<String> {
    fn sql_ident(&self) -> String {
        quote_identifier(self)
    }

    fn sql_ident_prefix(&self, pre: &str) -> String {
        match self {
            None => String::new(),
            Some(_) => format!("{}{}", pre, self.sql_ident()),
        }
    }

    fn sql_ident_suffix(&self, suf: &str) -> String {
        match self {
            None => String::new(),
            Some(_) => format!("{}{}", self.sql_ident(), suf),
        }
    }
}

impl SqlIdent for Option<Vec<Node>> {
    #[track_caller]
    fn sql_ident(&self) -> String {
        make_name(self).expect("unable to make SqlIdent")
    }

    #[track_caller]
    fn sql_ident_prefix(&self, pre: &str) -> String {
        match self {
            None => String::new(),
            Some(_) => format!("{}{}", pre, self.sql_ident()),
        }
    }

    #[track_caller]
    fn sql_ident_suffix(&self, suf: &str) -> String {
        match self {
            None => String::new(),
            Some(_) => format!("{}{}", self.sql_ident(), suf),
        }
    }
}

impl SqlIdent for Vec<Node> {
    #[track_caller]
    fn sql_ident(&self) -> String {
        make_name(&Some(self.clone())).expect("unable to make SqlIdent")
    }

    #[track_caller]
    fn sql_ident_prefix(&self, pre: &str) -> String {
        format!("{}{}", pre, self.sql_ident())
    }

    #[track_caller]
    fn sql_ident_suffix(&self, suf: &str) -> String {
        format!("{}{}", self.sql_ident(), suf)
    }
}

impl SqlIdent for Option<Box<Node>> {
    #[track_caller]
    fn sql_ident(&self) -> String {
        match self {
            None => String::new(),
            Some(node) => {
                make_name(&Some(vec![node.as_ref().clone()])).expect("unable to make SqlIdent")
            }
        }
    }

    #[track_caller]
    fn sql_ident_prefix(&self, pre: &str) -> String {
        match self {
            None => String::new(),
            Some(_) => format!("{}{}", pre, self.sql_ident()),
        }
    }

    #[track_caller]
    fn sql_ident_suffix(&self, suf: &str) -> String {
        match self {
            None => String::new(),
            Some(_) => format!("{}{}", self.sql_ident(), suf),
        }
    }
}

impl SqlIdent for Node {
    #[track_caller]
    fn sql_ident(&self) -> String {
        make_name(&Some(vec![self.clone()])).expect("unable to make SqlIdent")
    }

    #[track_caller]
    fn sql_ident_prefix(&self, pre: &str) -> String {
        format!("{}{}", pre, self.sql_ident())
    }

    #[track_caller]
    fn sql_ident_suffix(&self, suf: &str) -> String {
        format!("{}{}", suf, self.sql_ident())
    }
}

pub trait Len {
    fn len(&self) -> usize;
    fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

impl Len for Option<Vec<Node>> {
    fn len(&self) -> usize {
        self.as_ref().unwrap_or(&EMPTY_NODE_VEC).len()
    }
}

#[derive(Debug)]
pub struct SchemaSet {
    nodes: indexmap::IndexSet<DiffableStatement>,
}

impl Default for SchemaSet {
    fn default() -> Self {
        SchemaSet {
            nodes: Default::default(),
        }
    }
}

impl SchemaSet {
    pub fn new() -> Self {
        Default::default()
    }

    pub fn push(&mut self, sql: &str, node: Node) {
        #[inline]
        fn push(
            nodes: &mut indexmap::IndexSet<DiffableStatement>,
            sql: &str,
            node: Node,
            differ: impl Diff + 'static,
        ) {
            nodes.insert(DiffableStatement::new(sql, node, differ));
        }

        match node.clone() {
            Node::AlterCollationStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::AlterFunctionStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::AlterObjectSchemaStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::AlterOwnerStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::AlterTableStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::AlterTypeStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::ClusterStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CommentStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CompositeTypeStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CopyStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CreateAmStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CreateCastStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CreateConversionStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CreateDomainStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CreateEnumStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CreateForeignServerStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CreateForeignTableStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CreateFdwStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CreateFunctionStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CreateOpClassStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CreatePolicyStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CreateRangeStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CreateRoleStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CreateSeqStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CreateSchemaStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CreateStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CreateTableAsStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CreateTrigStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::DeclareCursorStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::DefineStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::DeleteStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::DiscardStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::DoStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::DropRoleStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::DropStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::ExplainStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::FetchStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::GrantRoleStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::GrantStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::IndexStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::InsertStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::ListenStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::LockStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::NotifyStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::PrepareStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::RenameStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::RuleStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::SelectStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::TransactionStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::TruncateStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::UnlistenStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::UpdateStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::VacuumStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::VariableSetStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::VariableShowStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::ViewStmt(stmt) => push(&mut self.nodes, sql, node, stmt),
            Node::CreateEventTrigStmt(stmt) => push(&mut self.nodes, sql, node, stmt),

            _ => println!("/*\nunknown node: {:?}\n\n{}\n*/", node, sql),
        };
    }

    pub fn scan_file(&mut self, filename: &str) {
        let mut sql =
            std::fs::read_to_string(filename).expect(&format!("failed to read file: {}", filename));
        sql = sql.replace("@extschema@", "\"@extschema@\"");
        let scanner = SqlStatementScanner::new(&sql);
        for stmt in scanner.into_iter() {
            match stmt.parsetree {
                Ok(parsetree) => {
                    if let Some(node) = parsetree {
                        self.push(stmt.sql, node);
                    }
                }

                // it couldn't be parsed -- panic!
                Err(e) => {
                    panic!("INPUT PARSE ERROR: {:?}\n{}\n/---", e, stmt.sql.trim());
                }
            };
        }
    }

    pub fn deparse(&self) -> String {
        let mut sql = String::new();

        for node in &self.nodes {
            if let Node::AlterTableStmt(_) = &node.node {
                println!("skipping AlterTableStmt");
                continue;
            }
            // println!("{}", node.sql);
            let deparsed = match catch_unwind(|| node.node.sql()) {
                Ok(deparsed) => deparsed,
                Err(e) => {
                    panic!(
                        "{:?}\n\n\nnode=\n{:#?}\nsql={}",
                        e,
                        node.node,
                        node.sql.trim(),
                    )
                }
            };
            let reparsed = parse_query(&deparsed).unwrap_or_else(|e| {
                panic!(
                    "FAILED TO REPARSE:\n{:#?}\n{:#?}\nORIG:\n   {}\nNEW:\n   {};",
                    e, node.node, node.sql, deparsed,
                )
            });
            if &node.node != reparsed.get(0).expect("didn't parse anything") {
                panic!(
                    "TREES NOT EQUAL:{:#?};\n---------\n{:#?};\n\n\nORIG:\n   {}\nNEW:\n   {};\n",
                    node.node,
                    reparsed.get(0).unwrap(),
                    node.sql,
                    deparsed
                );
            }

            sql.push_str("==================\n");
            sql.push_str(&format!("{}:\n{}\n", "BEFORE".yellow(), node.sql.trim()));
            sql.push_str(&format!("{}:\n{};\n", "AFTER".green(), deparsed.trim()));
            sql.push_str("/=================\n");
        }

        sql
    }

    pub fn diff(self, that: &SchemaSet) -> String {
        let mut sql = String::new();

        // Find objects in 'self' that don't exist in 'that' so we can DROP them
        for this_node in &self.nodes {
            if !that.nodes.contains(this_node) {
                match this_node.differ.drop_stmt() {
                    Some(drop) => {
                        sql.push_str(&drop);
                        sql.push_str(";\n");
                    }
                    None => {
                        // it's a statement that we don't know how to drop
                    }
                }
            }
        }

        // find objects that are either in both or only in 'that'
        for that_node in &that.nodes {
            // do we have that_node?
            match self.nodes.get(that_node) {
                // yes, we do have that node, so lets see if it's different
                Some(this_node) => {
                    if &this_node.node.sql() != &that_node.node.sql() {
                        // they are different, so we try to alter it
                        if let Some(alter) = this_node.differ.alter_stmt(&that_node.node) {
                            sql.push_str(&alter);
                            sql.push_str(";\n");
                        }
                    }
                }

                // no, we don't have that node, so we need to just its sql directly
                None => {
                    sql.push_str(&that_node.sql);
                    sql.push_str(";\n");
                }
            }
        }

        sql
    }

    /// Validate that `upgrade` contains the SQL needed to transition from
    /// `self` to `that`. For every object whose statement type implements
    /// `drop_stmt`/`alter_stmt` (i.e. has a non-empty `schema_object_identities`),
    /// we check that the upgrade script has a matching DROP, CREATE, or
    /// CREATE-OR-REPLACE keyed by identity. Returns a human-readable report.
    pub fn validate_upgrade(&self, that: &SchemaSet, upgrade: &SchemaSet) -> Result<(), String> {
        let mut upgrade_dropped: indexmap::IndexSet<String> = indexmap::IndexSet::new();
        let mut upgrade_created: indexmap::IndexSet<String> = indexmap::IndexSet::new();
        for stmt in &upgrade.nodes {
            let ids = stmt.differ.schema_object_identities();
            if ids.is_empty() {
                continue;
            }
            if matches!(&stmt.node, Node::DropStmt(_)) {
                upgrade_dropped.extend(ids);
            } else {
                upgrade_created.extend(ids);
            }
        }

        let mut missing: Vec<String> = Vec::new();

        for stmt in &self.nodes {
            if that.nodes.contains(stmt) {
                continue;
            }
            let ids = stmt.differ.schema_object_identities();
            if ids.is_empty() || ids.iter().all(|id| upgrade_dropped.contains(id)) {
                continue;
            }
            if let Some(drop) = stmt.differ.drop_stmt() {
                eprintln!("DROP missing for {}", ids.join(", "));
                missing.push(drop);
            }
        }

        for stmt in &that.nodes {
            if self.nodes.contains(stmt) {
                continue;
            }
            let ids = stmt.differ.schema_object_identities();
            if ids.is_empty() || ids.iter().all(|id| upgrade_created.contains(id)) {
                continue;
            }
            eprintln!("CREATE missing for {}", ids.join(", "));
            missing.push(stmt.sql.clone());
        }

        for that_stmt in &that.nodes {
            let Some(this_stmt) = self.nodes.get(that_stmt) else {
                continue;
            };
            if this_stmt.node.sql() == that_stmt.node.sql() {
                continue;
            }
            let ids = that_stmt.differ.schema_object_identities();
            // A modified object needs the upgrade to leave it in `that`'s
            // shape. CREATE OR REPLACE satisfies that on its own, and so does
            // DROP+CREATE — both put the id in `upgrade_created`. A bare DROP
            // does not: it removes the object that `that` expects to exist.
            if ids.is_empty() || ids.iter().all(|id| upgrade_created.contains(id)) {
                continue;
            }
            // Prefer the source-side's smarter alter (e.g. CREATE OR REPLACE
            // for functions); fall back to drop + new-create when the type
            // doesn't override `alter_stmt`.
            let suggested = this_stmt
                .differ
                .alter_stmt(&that_stmt.node)
                .unwrap_or_else(|| {
                    let drop = this_stmt
                        .differ
                        .drop_stmt()
                        .expect("schema_object_identities implies drop_stmt is implemented");
                    format!("{};\n{}", drop, that_stmt.sql)
                });
            eprintln!("ALTER missing for {};", ids.join(", "));
            missing.push(suggested.clone());
        }

        if missing.is_empty() {
            Ok(())
        } else {
            let mut report = String::new();
            for entry in &missing {
                report.push_str(entry);
                report.push('\n');
            }
            Err(report)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(sql: &str) -> SchemaSet {
        let mut set = SchemaSet::new();
        let scanner = SqlStatementScanner::new(sql);
        for stmt in scanner.into_iter() {
            match stmt.parsetree {
                Ok(Some(node)) => set.push(stmt.sql, node),
                Ok(None) => {}
                Err(e) => panic!("test fixture failed to parse: {:?}\n{}", e, stmt.sql),
            }
        }
        set
    }

    fn assert_validates(a: &str, b: &str, upgrade: &str) {
        let result = parse(a).validate_upgrade(&parse(b), &parse(upgrade));
        assert!(result.is_ok(), "expected valid, got missing:\n{}", result.unwrap_err());
    }

    fn assert_missing(a: &str, b: &str, upgrade: &str, expected_substring: &str) {
        let result = parse(a).validate_upgrade(&parse(b), &parse(upgrade));
        let err = result.expect_err("expected validation to fail");
        assert!(
            err.contains(expected_substring),
            "report did not contain {:?}; full report:\n{}",
            expected_substring,
            err
        );
    }

    #[test]
    fn no_changes_with_empty_upgrade_is_ok() {
        let schema = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 1';";
        assert_validates(schema, schema, "");
    }

    #[test]
    fn added_function_satisfied_by_create_in_upgrade() {
        let a = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 1';";
        let b = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 1';\n\
                 CREATE FUNCTION bar() RETURNS int LANGUAGE sql AS 'select 2';";
        let upgrade = "CREATE FUNCTION bar() RETURNS int LANGUAGE sql AS 'select 2';";
        assert_validates(a, b, upgrade);
    }

    #[test]
    fn added_function_missing_create_is_reported() {
        let a = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 1';";
        let b = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 1';\n\
                 CREATE FUNCTION bar() RETURNS int LANGUAGE sql AS 'select 2';";
        assert_missing(a, b, "", "bar");
    }

    #[test]
    fn dropped_function_satisfied_by_drop_in_upgrade() {
        let a = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 1';\n\
                 CREATE FUNCTION bar() RETURNS int LANGUAGE sql AS 'select 2';";
        let b = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 1';";
        let upgrade = "DROP FUNCTION bar();";
        assert_validates(a, b, upgrade);
    }

    #[test]
    fn dropped_function_missing_drop_is_reported() {
        let a = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 1';\n\
                 CREATE FUNCTION bar() RETURNS int LANGUAGE sql AS 'select 2';";
        let b = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 1';";
        assert_missing(a, b, "", "bar");
    }

    #[test]
    fn modified_function_satisfied_by_create_or_replace() {
        let a = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 1';";
        let b = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 2';";
        let upgrade = "CREATE OR REPLACE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 2';";
        assert_validates(a, b, upgrade);
    }

    #[test]
    fn modified_function_satisfied_by_drop_and_create() {
        let a = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 1';";
        let b = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 2';";
        let upgrade = "DROP FUNCTION foo(int);\n\
                       CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 2';";
        assert_validates(a, b, upgrade);
    }

    #[test]
    fn modified_function_missing_alter_is_reported() {
        let a = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 1';";
        let b = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 2';";
        assert_missing(a, b, "", "foo");
    }

    #[test]
    fn modified_function_drop_only_does_not_satisfy_alter() {
        // A bare DROP removes the object, but `b` expects it to exist with
        // the new body — the upgrade also needs a CREATE (or CREATE OR REPLACE).
        let a = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 1';";
        let b = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 2';";
        let upgrade = "DROP FUNCTION foo(int);";
        assert_missing(a, b, upgrade, "foo");
    }

    #[test]
    fn signature_change_needs_both_drop_and_create() {
        // foo(int) → foo(text) is a drop of one object and a create of another.
        let a = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 1';";
        let b = "CREATE FUNCTION foo(x text) RETURNS int LANGUAGE sql AS 'select 1';";
        let upgrade = "DROP FUNCTION foo(int);\n\
                       CREATE FUNCTION foo(x text) RETURNS int LANGUAGE sql AS 'select 1';";
        assert_validates(a, b, upgrade);
    }

    #[test]
    fn signature_change_with_only_create_misses_the_drop() {
        let a = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 1';";
        let b = "CREATE FUNCTION foo(x text) RETURNS int LANGUAGE sql AS 'select 1';";
        let upgrade = "CREATE FUNCTION foo(x text) RETURNS int LANGUAGE sql AS 'select 1';";
        // foo(int) still needs to be dropped — not yet covered by the upgrade.
        assert_missing(a, b, upgrade, "foo");
    }

    #[test]
    fn unrelated_objects_in_upgrade_do_not_help() {
        // upgrade adds an unrelated table/function — doesn't satisfy the missing
        // bar drop.
        let a = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 1';\n\
                 CREATE FUNCTION bar() RETURNS int LANGUAGE sql AS 'select 2';";
        let b = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 1';";
        let upgrade = "DROP FUNCTION foo(int);";
        assert_missing(a, b, upgrade, "bar");
    }

    #[test]
    fn enum_type_added_satisfied_by_create() {
        let a = "";
        let b = "CREATE TYPE color AS ENUM ('red', 'blue');";
        let upgrade = "CREATE TYPE color AS ENUM ('red', 'blue');";
        assert_validates(a, b, upgrade);
    }

    #[test]
    fn enum_type_dropped_satisfied_by_drop_type() {
        // CreateEnumStmt's identity is TYPE:name; DROP TYPE in the upgrade
        // must use the same identity to match.
        let a = "CREATE TYPE color AS ENUM ('red', 'blue');";
        let b = "";
        let upgrade = "DROP TYPE color;";
        assert_validates(a, b, upgrade);
    }

    // ---- gap coverage ----

    // 1. Other statement types

    #[test]
    fn schema_added_satisfied_by_create() {
        assert_validates("", "CREATE SCHEMA s;", "CREATE SCHEMA s;");
    }

    #[test]
    fn schema_dropped_satisfied_by_drop() {
        assert_validates("CREATE SCHEMA s;", "", "DROP SCHEMA s;");
    }

    #[test]
    fn view_added_satisfied_by_create() {
        let b = "CREATE VIEW v AS SELECT 1;";
        let upgrade = "CREATE VIEW v AS SELECT 1;";
        assert_validates("", b, upgrade);
    }

    #[test]
    fn view_modified_satisfied_by_drop_and_create() {
        let a = "CREATE VIEW v AS SELECT 1;";
        let b = "CREATE VIEW v AS SELECT 2;";
        let upgrade = "DROP VIEW v;\nCREATE VIEW v AS SELECT 2;";
        assert_validates(a, b, upgrade);
    }

    #[test]
    fn cast_added_satisfied_by_create() {
        let b = "CREATE CAST (int AS text) WITHOUT FUNCTION;";
        let upgrade = "CREATE CAST (int AS text) WITHOUT FUNCTION;";
        assert_validates("", b, upgrade);
    }

    #[test]
    fn cast_dropped_satisfied_by_drop() {
        let a = "CREATE CAST (int AS text) WITHOUT FUNCTION;";
        let upgrade = "DROP CAST (int AS text);";
        assert_validates(a, "", upgrade);
    }

    #[test]
    fn operator_added_satisfied_by_create() {
        let b = "CREATE OPERATOR === (LEFTARG = int, RIGHTARG = int, FUNCTION = int4eq);";
        let upgrade = "CREATE OPERATOR === (LEFTARG = int, RIGHTARG = int, FUNCTION = int4eq);";
        assert_validates("", b, upgrade);
    }

    #[test]
    fn operator_dropped_satisfied_by_drop() {
        let a = "CREATE OPERATOR === (LEFTARG = int, RIGHTARG = int, FUNCTION = int4eq);";
        let upgrade = "DROP OPERATOR === (int, int);";
        assert_validates(a, "", upgrade);
    }

    #[test]
    fn procedure_added_satisfied_by_create() {
        let b = "CREATE PROCEDURE p(x int) LANGUAGE sql AS $$ SELECT 1 $$;";
        let upgrade = "CREATE PROCEDURE p(x int) LANGUAGE sql AS $$ SELECT 1 $$;";
        assert_validates("", b, upgrade);
    }

    #[test]
    fn procedure_dropped_satisfied_by_drop() {
        let a = "CREATE PROCEDURE p(x int) LANGUAGE sql AS $$ SELECT 1 $$;";
        let upgrade = "DROP PROCEDURE p(int);";
        assert_validates(a, "", upgrade);
    }

    // 2. Function overloads are distinct identities

    #[test]
    fn function_overloads_are_independent_objects() {
        // Dropping foo(int) should not affect foo(text) — they share a name
        // but are different schema objects.
        let a = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 1';\n\
                 CREATE FUNCTION foo(x text) RETURNS int LANGUAGE sql AS 'select 2';";
        let b = "CREATE FUNCTION foo(x text) RETURNS int LANGUAGE sql AS 'select 2';";
        let upgrade = "DROP FUNCTION foo(int);";
        assert_validates(a, b, upgrade);
    }

    #[test]
    fn dropping_wrong_overload_does_not_satisfy() {
        // The upgrade drops foo(text), but the change requires dropping foo(int).
        let a = "CREATE FUNCTION foo(x int) RETURNS int LANGUAGE sql AS 'select 1';\n\
                 CREATE FUNCTION foo(x text) RETURNS int LANGUAGE sql AS 'select 2';";
        let b = "CREATE FUNCTION foo(x text) RETURNS int LANGUAGE sql AS 'select 2';";
        let upgrade = "DROP FUNCTION foo(text);";
        assert_missing(a, b, upgrade, "foo");
    }

    // 3. Out-of-scope statements don't cause false positives

    #[test]
    fn dropped_table_is_silently_ignored() {
        // CREATE TABLE has no `schema_object_identities` — the validator
        // only checks objects whose statement type opts in.
        assert_validates("CREATE TABLE t (id int);", "", "");
    }

    #[test]
    fn added_table_is_silently_ignored() {
        assert_validates("", "CREATE TABLE t (id int);", "");
    }

    // 4. Alter on a type without an `alter_stmt` override (CreateEnumStmt
    //    falls through to the trait default's drop+create suggestion)

    #[test]
    fn modified_enum_satisfied_by_drop_and_create() {
        let a = "CREATE TYPE color AS ENUM ('red', 'blue');";
        let b = "CREATE TYPE color AS ENUM ('red', 'blue', 'green');";
        let upgrade = "DROP TYPE color;\nCREATE TYPE color AS ENUM ('red', 'blue', 'green');";
        assert_validates(a, b, upgrade);
    }

    #[test]
    fn modified_enum_missing_alter_is_reported() {
        let a = "CREATE TYPE color AS ENUM ('red', 'blue');";
        let b = "CREATE TYPE color AS ENUM ('red', 'blue', 'green');";
        assert_missing(a, b, "", "color");
    }

    // 5. CREATE OR REPLACE in install scripts has the same identity as CREATE

    #[test]
    fn install_create_and_create_or_replace_share_identity() {
        // The two install scripts declare the "same" function with different
        // syntactic forms (CREATE vs CREATE OR REPLACE). Identity matches, so
        // the function isn't reported as added or dropped — but the rendered
        // SQL differs, so it surfaces as an alter that the upgrade satisfies.
        let a = "CREATE FUNCTION foo() RETURNS int LANGUAGE sql AS 'select 1';";
        let b = "CREATE OR REPLACE FUNCTION foo() RETURNS int LANGUAGE sql AS 'select 1';";
        let upgrade = "CREATE OR REPLACE FUNCTION foo() RETURNS int LANGUAGE sql AS 'select 1';";
        assert_validates(a, b, upgrade);
    }
}
