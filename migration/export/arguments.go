//go:build migration_export

package main

import (
	"fmt"
	"github.com/antisynthesis/asc-mcp/internal/asc/mcp"
	"go/ast"
	"go/parser"
	"go/token"
	"path/filepath"
	"reflect"
	"strconv"
	"strings"
)

func handlerArguments() map[string]map[string]mcp.Property {
	files, _ := filepath.Glob("internal/asc/tools/*.go")
	named := map[string]*ast.StructType{}
	funcs := map[string]*ast.FuncDecl{}
	handlers := map[string]string{}
	for _, path := range files {
		if strings.HasSuffix(path, "_test.go") {
			continue
		}
		f, e := parser.ParseFile(token.NewFileSet(), path, nil, 0)
		if e != nil {
			panic(e)
		}
		for _, d := range f.Decls {
			switch d := d.(type) {
			case *ast.FuncDecl:
				funcs[d.Name.Name] = d
			case *ast.GenDecl:
				for _, sp := range d.Specs {
					if t, ok := sp.(*ast.TypeSpec); ok {
						if st, ok := t.Type.(*ast.StructType); ok {
							named[t.Name.Name] = st
						}
					}
				}
			}
		}
		ast.Inspect(f, func(n ast.Node) bool {
			call, ok := n.(*ast.CallExpr)
			if !ok || len(call.Args) < 2 {
				return true
			}
			sel, ok := call.Fun.(*ast.SelectorExpr)
			if !ok || sel.Sel.Name != "register" {
				return true
			}
			lit, ok := call.Args[0].(*ast.CompositeLit)
			if !ok {
				return true
			}
			name := ""
			for _, elt := range lit.Elts {
				if kv, ok := elt.(*ast.KeyValueExpr); ok {
					if k, ok := kv.Key.(*ast.Ident); ok && k.Name == "Name" {
						if v, ok := kv.Value.(*ast.BasicLit); ok {
							name, _ = strconv.Unquote(v.Value)
						}
					}
				}
			}
			if h, ok := call.Args[len(call.Args)-1].(*ast.SelectorExpr); ok && name != "" {
				handlers[name] = h.Sel.Name
			}
			return true
		})
	}
	var fields func(*ast.StructType) map[string]mcp.Property
	var prop func(ast.Expr) mcp.Property
	prop = func(t ast.Expr) mcp.Property {
		switch t := t.(type) {
		case *ast.StarExpr:
			return prop(t.X)
		case *ast.Ident:
			switch t.Name {
			case "bool":
				return mcp.Property{Type: "boolean"}
			case "int", "int64", "uint64":
				return mcp.Property{Type: "integer"}
			case "float64":
				return mcp.Property{Type: "number"}
			case "string":
				return mcp.Property{Type: "string"}
			default:
				return mcp.Property{Type: "object"}
			}
		case *ast.ArrayType:
			item := prop(t.Elt)
			return mcp.Property{Type: "array", Items: &item}
		default:
			return mcp.Property{Type: "object"}
		}
	}
	fields = func(st *ast.StructType) map[string]mcp.Property {
		out := map[string]mcp.Property{}
		for _, f := range st.Fields.List {
			if len(f.Names) == 0 {
				if id, ok := f.Type.(*ast.Ident); ok {
					if n := named[id.Name]; n != nil {
						for k, p := range fields(n) {
							out[k] = p
						}
					}
				}
				continue
			}
			if f.Tag == nil {
				continue
			}
			tag, _ := strconv.Unquote(f.Tag.Value)
			name := strings.Split(reflect.StructTag(tag).Get("json"), ",")[0]
			if name != "" && name != "-" {
				out[name] = prop(f.Type)
			}
		}
		return out
	}
	out := map[string]map[string]mcp.Property{}
	for name, handler := range handlers {
		f := funcs[handler]
		if f == nil {
			continue
		}
		ast.Inspect(f.Body, func(n ast.Node) bool {
			v, ok := n.(*ast.ValueSpec)
			if !ok {
				return true
			}
			for _, id := range v.Names {
				if id.Name == "params" {
					if st, ok := v.Type.(*ast.StructType); ok {
						out[name] = fields(st)
					} else if ident, ok := v.Type.(*ast.Ident); ok && named[ident.Name] != nil {
						out[name] = fields(named[ident.Name])
					}
				}
			}
			return true
		})
	}
	fmt.Println("handler parameter structs", len(out), "registered handlers", len(handlers))
	return out
}
