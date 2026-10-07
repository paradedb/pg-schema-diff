// Copyright 2020-2026 Eric B. Ridge <eebbrr@gmail.com>. All rights reserved. Use
// of this source code is governed by the Postgres license that can be found in
// the LICENSE file.
use pg_query::{protobuf as pb, Node, NodeEnum};
use serde_json::Value;

#[derive(Debug)]
pub struct Statement {
    pub source: String,
    pub node: Node,
}

impl Statement {
    pub fn new(source: &str, mut node: Node) -> Self {
        normalize_function_options(&mut node);
        Self {
            source: source.into(),
            node,
        }
    }

    fn kind(&self) -> &NodeEnum {
        self.node.node.as_ref().expect("statement has no node")
    }

    pub fn sql(&self) -> String {
        // libpg_query can lose identifier quotes or change expression grouping.
        // Only use its output if it parses back to the same tree.
        if let Ok(sql) = self.node.deparse() {
            if same_tree(&self.node, &sql) {
                return sql;
            }
        }
        assert!(
            same_tree(&self.node, &self.source),
            "source SQL does not match statement AST"
        );
        self.source.clone()
    }

    pub fn identifier(&self) -> String {
        let named = match self.kind() {
            NodeEnum::CreateFunctionStmt(stmt) => return function_drop(stmt),
            NodeEnum::CreateStmt(stmt) => {
                Some(("RELATION", relation_name(stmt.relation.as_ref().unwrap())))
            }
            NodeEnum::ViewStmt(stmt) => Some(("VIEW", relation_name(stmt.view.as_ref().unwrap()))),
            NodeEnum::CreateSchemaStmt(stmt) => Some(("SCHEMA", schema_name(stmt))),
            NodeEnum::CreateEnumStmt(stmt) => Some(("ENUM", qualified_name(&stmt.type_name))),
            NodeEnum::CreateDomainStmt(stmt) => Some(("DOMAIN", qualified_name(&stmt.domainname))),
            NodeEnum::CreateAmStmt(stmt) => Some(("ACCESS METHOD", quote_identifier(&stmt.amname))),
            NodeEnum::CreateEventTrigStmt(stmt) => {
                Some(("EVENT TRIGGER", quote_identifier(&stmt.trigname)))
            }
            NodeEnum::CreateOpClassStmt(stmt) => Some((
                "OPERATOR CLASS",
                format!(
                    "{} USING {}",
                    qualified_name(&stmt.opclassname),
                    quote_identifier(&stmt.amname)
                ),
            )),
            NodeEnum::DefineStmt(stmt) => match pb::ObjectType::try_from(stmt.kind).unwrap() {
                // Shell/full types and distinct operator definitions must coexist.
                pb::ObjectType::ObjectType | pb::ObjectType::ObjectOperator => return self.sql(),
                pb::ObjectType::ObjectAggregate => return define_drop(stmt),
                kind => {
                    return format!("{}:{}", kind.as_str_name(), qualified_name(&stmt.defnames))
                }
            },
            _ => None,
        };
        match named {
            Some((kind, name)) => format!("{kind}:{name}"),
            None => canonical_tree(&self.node).to_string(),
        }
    }

    pub fn drop_stmt(&self) -> Option<String> {
        match self.kind() {
            NodeEnum::CreateFunctionStmt(stmt) => Some(function_drop(stmt)),
            NodeEnum::ViewStmt(stmt) => Some(format!(
                "DROP VIEW {}",
                relation_name(stmt.view.as_ref().unwrap())
            )),
            NodeEnum::CreateSchemaStmt(stmt) => Some(format!("DROP SCHEMA {}", schema_name(stmt))),
            NodeEnum::CreateEnumStmt(stmt) => {
                Some(name_drop(pb::ObjectType::ObjectType, &stmt.type_name))
            }
            NodeEnum::CreateCastStmt(stmt) => Some(drop_sql(
                pb::ObjectType::ObjectCast,
                vec![list(vec![
                    node(NodeEnum::TypeName(stmt.sourcetype.clone().unwrap())),
                    node(NodeEnum::TypeName(stmt.targettype.clone().unwrap())),
                ])],
            )),
            NodeEnum::DefineStmt(stmt) => Some(define_drop(stmt)),
            NodeEnum::GrantStmt(stmt) => {
                let mut inverse = stmt.clone();
                inverse.is_grant = !inverse.is_grant;
                inverse.grant_option = false;
                Some(NodeEnum::GrantStmt(inverse).deparse().unwrap())
            }
            NodeEnum::DoStmt(_)
            | NodeEnum::InsertStmt(_)
            | NodeEnum::AlterFunctionStmt(_)
            | NodeEnum::AlterTypeStmt(_) => None,
            _ => unimplemented!("Don't know how to drop: {:?}", self.node),
        }
    }

    pub fn alter_stmt(&self, other: &Self) -> Option<String> {
        match (self.kind(), other.kind()) {
            (NodeEnum::CreateFunctionStmt(_), NodeEnum::CreateFunctionStmt(stmt)) => {
                let mut replacement = stmt.clone();
                replacement.replace = true;
                let source = if stmt.replace {
                    other.source.clone()
                } else {
                    let tokens = pg_query::scan(&other.source).unwrap();
                    let end = tokens
                        .tokens
                        .iter()
                        .find(|token| token.token == pb::Token::Create as i32)
                        .expect("function source must contain CREATE")
                        .end as usize;
                    format!(
                        "{} OR REPLACE{}",
                        &other.source[..end],
                        &other.source[end..]
                    )
                };
                let replacement =
                    Statement::new(&source, node(NodeEnum::CreateFunctionStmt(replacement)));
                Some(format!(
                    "{};\n{}",
                    self.drop_stmt().unwrap(),
                    replacement.sql()
                ))
            }
            (NodeEnum::ViewStmt(_), NodeEnum::ViewStmt(_)) => {
                Some(format!("{};\n{}", self.drop_stmt().unwrap(), other.sql()))
            }
            (NodeEnum::CreateSchemaStmt(before), NodeEnum::CreateSchemaStmt(after)) => {
                if before.authrole != after.authrole {
                    after.authrole.as_ref().map(|role| {
                        NodeEnum::AlterOwnerStmt(Box::new(pb::AlterOwnerStmt {
                            object_type: pb::ObjectType::ObjectSchema as i32,
                            object: Some(Box::new(string_node(&after.schemaname))),
                            newowner: Some(role.clone()),
                            ..Default::default()
                        }))
                        .deparse()
                        .unwrap()
                    })
                } else {
                    None
                }
            }
            (NodeEnum::DefineStmt(before), NodeEnum::DefineStmt(after)) => {
                alter_operator(before, after)
            }
            _ => {
                println!(
                    "/*\nDon't know how to ALTER:\n{}\n{:?}\n{:?}\n*/",
                    self.sql(),
                    other.node,
                    self.node
                );
                None
            }
        }
    }
}

fn same_tree(node: &Node, sql: &str) -> bool {
    match pg_query::parse(sql) {
        Ok(parsed) if parsed.protobuf.stmts.len() == 1 => {
            canonical_tree(node) == canonical_tree(parsed.protobuf.stmts[0].stmt.as_ref().unwrap())
        }
        _ => false,
    }
}

fn normalize_function_options(node: &mut Node) {
    if let Some(NodeEnum::CreateFunctionStmt(stmt)) = &mut node.node {
        stmt.options.sort_by_key(|option| match &option.node {
            Some(NodeEnum::DefElem(elem)) => elem.defname.clone(),
            _ => String::new(),
        });
    }
}

// Source positions change during deparse/reparse; they are not schema differences.
pub fn canonical_tree(node: &Node) -> Value {
    fn strip_positions(value: &mut Value) {
        match value {
            Value::Object(fields) => {
                fields.retain(|key, value| {
                    !(value.is_number()
                        && (key == "location" || key.ends_with("_location") || key == "stmt_len"))
                });
                for child in fields.values_mut() {
                    strip_positions(child);
                }
            }
            Value::Array(items) => {
                for child in items {
                    strip_positions(child);
                }
            }
            _ => {}
        }
    }
    let mut node = node.clone();
    normalize_function_options(&mut node);
    let mut value = serde_json::to_value(node).unwrap();
    strip_positions(&mut value);
    value
}

fn node(kind: NodeEnum) -> Node {
    Node { node: Some(kind) }
}
fn list(items: Vec<Node>) -> Node {
    node(NodeEnum::List(pb::List { items }))
}
fn string_node(value: &str) -> Node {
    node(NodeEnum::String(pb::String { sval: value.into() }))
}

fn quote_identifier(name: &str) -> String {
    format!("\"{}\"", name.replace('"', "\"\""))
}

fn qualified_name(names: &[Node]) -> String {
    names
        .iter()
        .map(|name| match &name.node {
            Some(NodeEnum::String(value)) => quote_identifier(&value.sval),
            _ => panic!("expected identifier: {name:?}"),
        })
        .collect::<Vec<_>>()
        .join(".")
}

fn relation_name(relation: &pb::RangeVar) -> String {
    [
        &relation.catalogname,
        &relation.schemaname,
        &relation.relname,
    ]
    .into_iter()
    .filter(|part| !part.is_empty())
    .map(|part| quote_identifier(part))
    .collect::<Vec<_>>()
    .join(".")
}

fn schema_name(stmt: &pb::CreateSchemaStmt) -> String {
    if !stmt.schemaname.is_empty() {
        quote_identifier(&stmt.schemaname)
    } else {
        quote_identifier(
            &stmt
                .authrole
                .as_ref()
                .expect("schema requires a name or owner")
                .rolename,
        )
    }
}

fn drop_sql(kind: pb::ObjectType, objects: Vec<Node>) -> String {
    NodeEnum::DropStmt(pb::DropStmt {
        objects,
        remove_type: kind as i32,
        behavior: pb::DropBehavior::DropRestrict as i32,
        missing_ok: true,
        ..Default::default()
    })
    .deparse()
    .expect("failed to deparse DROP statement")
}

fn name_drop(kind: pb::ObjectType, names: &[Node]) -> String {
    let object = match kind {
        pb::ObjectType::ObjectType | pb::ObjectType::ObjectDomain => {
            node(NodeEnum::TypeName(pb::TypeName {
                names: names.to_vec(),
                typemod: -1,
                ..Default::default()
            }))
        }
        _ => list(names.to_vec()),
    };
    drop_sql(kind, vec![object])
}

fn function_drop(stmt: &pb::CreateFunctionStmt) -> String {
    let args = stmt
        .parameters
        .iter()
        .filter_map(|parameter| match &parameter.node {
            Some(NodeEnum::FunctionParameter(parameter))
                if !matches!(
                    pb::FunctionParameterMode::try_from(parameter.mode).unwrap(),
                    pb::FunctionParameterMode::FuncParamOut
                        | pb::FunctionParameterMode::FuncParamTable
                ) =>
            {
                Some(node(NodeEnum::TypeName(
                    parameter.arg_type.clone().unwrap(),
                )))
            }
            _ => None,
        })
        .collect();
    drop_sql(
        if stmt.is_procedure {
            pb::ObjectType::ObjectProcedure
        } else {
            pb::ObjectType::ObjectFunction
        },
        vec![node(NodeEnum::ObjectWithArgs(pb::ObjectWithArgs {
            objname: stmt.funcname.clone(),
            objargs: args,
            ..Default::default()
        }))],
    )
}

fn operator_args(stmt: &pb::DefineStmt) -> Vec<Node> {
    ["leftarg", "rightarg"]
        .into_iter()
        .map(|name| {
            stmt.definition
                .iter()
                .find_map(|node| match &node.node {
                    Some(NodeEnum::DefElem(elem)) if elem.defname == name => {
                        elem.arg.as_deref().cloned()
                    }
                    _ => None,
                })
                .unwrap_or_default()
        })
        .collect()
}

fn define_drop(stmt: &pb::DefineStmt) -> String {
    let kind = pb::ObjectType::try_from(stmt.kind).unwrap();
    match kind {
        pb::ObjectType::ObjectOperator => drop_sql(
            kind,
            vec![node(NodeEnum::ObjectWithArgs(pb::ObjectWithArgs {
                objname: stmt.defnames.clone(),
                objargs: operator_args(stmt),
                ..Default::default()
            }))],
        ),
        pb::ObjectType::ObjectAggregate => {
            let args = stmt
                .args
                .first()
                .and_then(|node| match &node.node {
                    Some(NodeEnum::List(list)) => Some(
                        list.items
                            .iter()
                            .filter_map(|node| match &node.node {
                                Some(NodeEnum::FunctionParameter(param)) => {
                                    Some(node_from_type(param.arg_type.clone().unwrap()))
                                }
                                _ => None,
                            })
                            .collect(),
                    ),
                    _ => None,
                })
                .unwrap_or_default();
            drop_sql(
                kind,
                vec![node(NodeEnum::ObjectWithArgs(pb::ObjectWithArgs {
                    objname: stmt.defnames.clone(),
                    objargs: args,
                    ..Default::default()
                }))],
            )
        }
        _ => name_drop(kind, &stmt.defnames),
    }
}

fn node_from_type(ty: pb::TypeName) -> Node {
    node(NodeEnum::TypeName(ty))
}

fn alter_operator(before: &pb::DefineStmt, after: &pb::DefineStmt) -> Option<String> {
    if before.kind != pb::ObjectType::ObjectOperator as i32 {
        return None;
    }
    let find = |stmt: &pb::DefineStmt, name: &str| {
        stmt.definition.iter().find_map(|node| match &node.node {
            Some(NodeEnum::DefElem(elem)) if elem.defname == name => Some(elem.as_ref().clone()),
            _ => None,
        })
    };
    let mut options = Vec::new();
    for name in ["restrict", "join"] {
        let old = find(before, name);
        let new = find(after, name);
        let canonical =
            |elem: &pb::DefElem| canonical_tree(&node(NodeEnum::DefElem(Box::new(elem.clone()))));
        if old.as_ref().map(canonical) != new.as_ref().map(canonical) {
            options.push(node(NodeEnum::DefElem(Box::new(new.unwrap_or_else(
                || pb::DefElem {
                    defname: name.into(),
                    ..Default::default()
                },
            )))));
        }
    }
    if options.is_empty() {
        return None;
    }
    Some(
        NodeEnum::AlterOperatorStmt(pb::AlterOperatorStmt {
            opername: Some(pb::ObjectWithArgs {
                objname: before.defnames.clone(),
                objargs: operator_args(before),
                ..Default::default()
            }),
            options,
        })
        .deparse()
        .unwrap(),
    )
}
