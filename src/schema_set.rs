// Copyright 2020-2026 Eric B. Ridge <eebbrr@gmail.com>. All rights reserved. Use
// of this source code is governed by the Postgres license that can be found in
// the LICENSE file.
use crate::nodes::{canonical_tree, Statement};
use colored::Colorize;

#[derive(Debug, Default)]
pub struct SchemaSet {
    nodes: indexmap::IndexMap<String, Statement>,
}

impl SchemaSet {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn scan_sql(&mut self, sql: &str) {
        let parsed =
            pg_query::parse(sql).unwrap_or_else(|e| panic!("INPUT PARSE ERROR: {e}\n{sql}\n/---"));
        for raw in parsed.protobuf.stmts {
            let start = raw.stmt_location as usize;
            let end = if raw.stmt_len == 0 {
                sql.len()
            } else {
                start + raw.stmt_len as usize
            };
            // PostgreSQL reports byte offsets, including for UTF-8 input.
            let source = sql[start..end].trim().trim_end_matches(';').trim();
            let node = *raw.stmt.expect("parser returned an empty statement");
            let statement = Statement::new(source, node);
            self.nodes
                .entry(statement.identifier())
                .or_insert(statement);
        }
    }

    pub fn scan_file(&mut self, filename: &str) {
        let sql = std::fs::read_to_string(filename)
            .unwrap_or_else(|e| panic!("failed to read file {filename}: {e}"));
        self.scan_sql(&sql.replace("@extschema@", "\"@extschema@\""));
    }

    pub fn deparse(&self) -> String {
        let mut sql = String::new();
        for statement in self.nodes.values() {
            let deparsed = statement.sql();
            let reparsed = pg_query::parse(&deparsed)
                .unwrap_or_else(|e| panic!("FAILED TO REPARSE: {e}\n{deparsed}"));
            assert_eq!(reparsed.protobuf.stmts.len(), 1);
            assert_eq!(
                canonical_tree(&statement.node),
                canonical_tree(reparsed.protobuf.stmts[0].stmt.as_ref().unwrap()),
                "TREES NOT EQUAL:\nORIG: {}\nNEW: {deparsed}",
                statement.source,
            );
            sql.push_str("==================\n");
            sql.push_str(&format!("{}:\n{}\n", "BEFORE".yellow(), statement.source));
            sql.push_str(&format!("{}:\n{};\n", "AFTER".green(), deparsed));
            sql.push_str("/=================\n");
        }
        sql
    }

    pub fn diff(&self, that: &Self) -> String {
        let mut sql = String::new();
        // Preserve drop-before-create ordering and source SQL for newly added objects.
        for (identifier, statement) in &self.nodes {
            if !that.nodes.contains_key(identifier) {
                if let Some(drop) = statement.drop_stmt() {
                    sql.push_str(&drop);
                    sql.push_str(";\n");
                }
            }
        }
        for (identifier, statement) in &that.nodes {
            match self.nodes.get(identifier) {
                Some(before) if canonical_tree(&before.node) != canonical_tree(&statement.node) => {
                    if let Some(alter) = before.alter_stmt(statement) {
                        sql.push_str(&alter);
                        sql.push_str(";\n");
                    }
                }
                Some(_) => {}
                None => {
                    sql.push_str(&statement.source);
                    sql.push_str(";\n");
                }
            }
        }
        sql
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn schema(sql: &str) -> SchemaSet {
        let mut set = SchemaSet::new();
        set.scan_sql(sql);
        set
    }

    fn diff(before: &str, after: &str) -> String {
        let output = schema(before).diff(&schema(after));
        pg_query::parse(&output).expect("diff must produce valid PostgreSQL SQL");
        output
    }

    #[test]
    fn roundtrips_schema_statements() {
        let statements = [
            "CREATE TABLE public.docs (id bigint PRIMARY KEY, body text, tags text[])",
            "CREATE INDEX docs_body ON public.docs USING gin (to_tsvector('english', body))",
            "CREATE VIEW public.v AS SELECT id, body FROM public.docs WHERE id > 0",
            "CREATE SCHEMA app AUTHORIZATION owner",
            "CREATE TYPE app.status AS ENUM ('new', 'it''s done')",
            "CREATE TYPE app.pair AS (x integer, y text)",
            "CREATE DOMAIN app.positive AS integer CHECK (VALUE > 0)",
            "CREATE SEQUENCE app.ids INCREMENT BY 3 START WITH 10",
            "CREATE CAST (integer AS text) WITH INOUT AS ASSIGNMENT",
            "GRANT SELECT, UPDATE (body) ON TABLE public.docs TO reader WITH GRANT OPTION",
            "REVOKE GRANT OPTION FOR SELECT ON TABLE public.docs FROM reader",
            "CREATE FUNCTION app.f(x integer DEFAULT 3) RETURNS integer LANGUAGE sql IMMUTABLE PARALLEL SAFE AS $$SELECT x + 1$$",
            "CREATE PROCEDURE app.p(IN x integer, OUT y integer) LANGUAGE plpgsql AS $$BEGIN y := x; END$$",
            "CREATE OPERATOR app.@@@ (FUNCTION = app.matches, LEFTARG = text, RIGHTARG = text, RESTRICT = contsel)",
            "CREATE AGGREGATE app.total(integer) (SFUNC = int4pl, STYPE = integer, INITCOND = '0')",
            "ALTER TABLE public.docs ADD COLUMN created_at timestamp DEFAULT now()",
            "DO $$BEGIN RAISE NOTICE 'a;b'; END$$",
        ];
        for sql in statements {
            assert!(schema(sql).deparse().contains("AFTER:"), "{sql}");
        }
    }

    #[test]
    fn supports_postgres_17_syntax() {
        let sql = "MERGE INTO docs USING incoming ON docs.id = incoming.id WHEN MATCHED THEN UPDATE SET body = incoming.body RETURNING docs.id; CREATE FUNCTION f() RETURNS integer LANGUAGE SQL RETURN 42";
        assert_eq!(schema(sql).nodes.len(), 2);
        schema(sql).deparse();
    }

    #[test]
    fn splits_utf8_comments_and_dollar_quoted_bodies() {
        let sql = "-- café\nCREATE FUNCTION f() RETURNS text LANGUAGE sql AS $body$SELECT 'héllo;world'$body$; /* résumé */ CREATE VIEW \"café\" AS SELECT 'a;b' AS value;";
        let set = schema(sql);
        assert_eq!(set.nodes.len(), 2);
        assert!(set
            .nodes
            .values()
            .nth(1)
            .unwrap()
            .source
            .contains("CREATE VIEW"));
        set.deparse();
    }

    #[test]
    fn whitespace_comments_and_positions_do_not_change_identity() {
        let before = "CREATE TABLE t(id integer); GRANT SELECT ON t TO reader;";
        let after =
            "-- moved\nCREATE TABLE t ( id INTEGER );\n/* padding */ GRANT SELECT ON t TO reader;";
        assert!(diff(before, after).is_empty());
    }

    #[test]
    fn new_objects_keep_source_sql_and_order() {
        let sql = "CREATE TABLE t(id integer); CREATE INDEX i ON t(id)";
        assert_eq!(
            diff("", sql),
            "CREATE TABLE t(id integer);\nCREATE INDEX i ON t(id);\n"
        );
    }

    #[test]
    fn narrowing_grants_revokes_before_granting() {
        let output = diff(
            "GRANT SELECT, INSERT ON t TO reader",
            "GRANT SELECT ON t TO reader",
        );
        assert!(
            output.to_uppercase().starts_with("REVOKE SELECT, INSERT"),
            "{output}"
        );
        assert!(output.contains(";\nGRANT SELECT"));
    }

    #[test]
    fn removing_revoke_restores_grant() {
        assert!(diff("REVOKE SELECT ON t FROM reader", "")
            .to_uppercase()
            .starts_with("GRANT SELECT"));
    }

    #[test]
    fn function_changes_drop_and_replace() {
        let output = diff(
            "CREATE FUNCTION app.f(x integer DEFAULT 1) RETURNS integer LANGUAGE sql AS $$SELECT x$$",
            "CREATE FUNCTION app.f(x integer DEFAULT 2) RETURNS integer LANGUAGE sql AS $$SELECT x + 1$$",
        );
        assert!(
            output.starts_with("DROP FUNCTION IF EXISTS app.f(int);"),
            "{output}"
        );
        assert!(output.contains("CREATE OR REPLACE FUNCTION"));
        assert!(!output.lines().next().unwrap().contains("DEFAULT"));
    }

    #[test]
    fn overloads_are_distinct_and_drop_only_input_types() {
        let before = "CREATE FUNCTION f(x integer, OUT y integer) LANGUAGE sql AS $$SELECT x$$; CREATE FUNCTION f(x text) RETURNS text LANGUAGE sql AS $$SELECT x$$";
        let after = "CREATE FUNCTION f(x text) RETURNS text LANGUAGE sql AS $$SELECT x$$";
        assert_eq!(schema(before).nodes.len(), 2);
        assert_eq!(diff(before, after), "DROP FUNCTION IF EXISTS f(int);\n");
    }

    #[test]
    fn table_return_columns_are_not_function_identity_arguments() {
        assert_eq!(diff("CREATE FUNCTION f(x integer) RETURNS TABLE(y integer) LANGUAGE sql AS $$SELECT x$$", ""), "DROP FUNCTION IF EXISTS f(int);\n");
    }

    #[test]
    fn procedures_use_procedure_drop() {
        assert_eq!(
            diff(
                "CREATE PROCEDURE p(INOUT x integer) LANGUAGE plpgsql AS $$BEGIN x := x + 1; END$$",
                ""
            ),
            "DROP PROCEDURE IF EXISTS p(int);\n"
        );
    }

    #[test]
    fn function_option_order_is_ignored() {
        assert!(diff(
            "CREATE FUNCTION f() RETURNS integer LANGUAGE sql IMMUTABLE AS $$SELECT 1$$",
            "CREATE FUNCTION f() RETURNS integer IMMUTABLE AS $$SELECT 1$$ LANGUAGE sql",
        )
        .is_empty());
    }

    #[test]
    fn view_changes_drop_before_recreating() {
        let output = diff(
            "CREATE VIEW v AS SELECT 1 AS x",
            "CREATE VIEW v AS SELECT 2 AS x",
        );
        assert!(output.starts_with("DROP VIEW \"v\";\nCREATE VIEW v"));
    }

    #[test]
    fn schema_owner_uses_new_role_and_quoted_name() {
        let output = diff(
            "CREATE SCHEMA \"Odd schema\" AUTHORIZATION old_owner",
            "CREATE SCHEMA \"Odd schema\" AUTHORIZATION new_owner",
        );
        assert_eq!(output, "ALTER SCHEMA \"Odd schema\" OWNER TO new_owner;\n");
    }

    #[test]
    fn enum_and_cast_drops_are_valid() {
        assert_eq!(
            diff("CREATE TYPE \"Odd schema\".status AS ENUM ('new')", ""),
            "DROP TYPE IF EXISTS \"Odd schema\".status;\n"
        );
        assert_eq!(
            diff("CREATE CAST (integer AS text) WITH INOUT", ""),
            "DROP CAST IF EXISTS (int AS text);\n"
        );
    }

    #[test]
    fn operator_drop_keeps_operator_unquoted() {
        let output = diff(
            "CREATE OPERATOR app.@@@ (FUNCTION = app.matches, LEFTARG = text, RIGHTARG = text)",
            "",
        );
        assert_eq!(output, "DROP OPERATOR IF EXISTS app.@@@(text, text);\n");
    }

    #[test]
    fn unary_operator_drop_uses_none() {
        let output = diff(
            "CREATE OPERATOR app.! (FUNCTION = app.factorial, RIGHTARG = integer)",
            "",
        );
        assert_eq!(output, "DROP OPERATOR IF EXISTS app.!(NONE, int);\n");
    }

    #[test]
    fn aggregate_drop_includes_signature() {
        assert_eq!(
            diff(
                "CREATE AGGREGATE app.total(integer) (SFUNC = int4pl, STYPE = integer)",
                ""
            ),
            "DROP AGGREGATE IF EXISTS app.total(int);\n"
        );
    }

    #[test]
    fn shell_and_full_types_coexist() {
        assert_eq!(schema("CREATE TYPE app.custom; CREATE TYPE app.custom (INPUT = app.input, OUTPUT = app.output)").nodes.len(), 2);
    }

    #[test]
    fn repeat_statements_are_deduplicated() {
        assert_eq!(
            schema("GRANT SELECT ON t TO reader; GRANT SELECT ON t TO reader")
                .nodes
                .len(),
            1
        );
    }

    #[test]
    fn quoted_keyword_parameters_roundtrip_and_replace_safely() {
        let before = "-- header\nCREATE FUNCTION f(\"limit\" integer, \"json\" json) RETURNS integer LANGUAGE sql AS $$SELECT 1$$";
        let after = "-- new header\nCREATE FUNCTION f(\"limit\" integer, \"json\" json) RETURNS integer LANGUAGE sql AS $$SELECT 2$$";
        schema(before).deparse();
        let output = diff(before, after);
        assert!(
            output.contains("CREATE OR REPLACE FUNCTION f(\"limit\" integer, \"json\" json)"),
            "{output}"
        );
        assert!(diff(before, &before.replace("-- header", "-- moved header")).is_empty());
    }

    #[test]
    fn tablespace_path_is_not_treated_as_a_source_position() {
        assert_ne!(
            schema("CREATE TABLESPACE ts LOCATION '/data/one'")
                .nodes
                .keys()
                .next(),
            schema("CREATE TABLESPACE ts LOCATION '/data/two'")
                .nodes
                .keys()
                .next(),
        );
    }

    #[test]
    fn replacement_function_option_keywords_roundtrip() {
        for options in [
            "SECURITY DEFINER",
            "SECURITY INVOKER",
            "LEAKPROOF",
            "WINDOW",
            "SET search_path = pg_catalog",
        ] {
            schema(&format!(
                "CREATE FUNCTION f() RETURNS integer LANGUAGE sql {options} AS $$SELECT 1$$"
            ))
            .deparse();
        }
    }

    #[test]
    #[should_panic(expected = "INPUT PARSE ERROR")]
    fn invalid_sql_is_rejected() {
        schema("CREATE TABLE (");
    }
}
