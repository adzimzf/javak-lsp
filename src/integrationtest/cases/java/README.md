# Java cases and the jdtls tests they come from

Most cases here were derived from the handler tests of jdtls
(`org.eclipse.jdt.ls.tests/src/org/eclipse/jdt/ls/core/internal/handlers/`
in [eclipse-jdtls/eclipse.jdt.ls](https://github.com/eclipse-jdtls/eclipse.jdt.ls)).
A case file has no room for a comment — everything above `//= expected` is
the fixture — so this file says where each one comes from.

- A case listed with a jdtls test asks what that test asks, on sources small
  enough to read in the case. Where the jdtls test leans on the JDK or a
  library (`println`, `new RuntimeException`, `StringUtils`), the fixture
  declares a type of its own to play the part.
- A case listed without one asks the same jdtls handler about a kind of name
  its tests leave out; its answer follows from how that handler is written.
- The expectations are what jdtls answers, written without looking at what
  javak-lsp answers. A case that fails is a difference from jdtls: fix the
  server, or show with a jdtls run that the expectation is wrong.

Notes on how an answer is written down:

- **references** always ask for the declaration too. No field has a
  `getX`/`setX` beside it: jdtls counts calls of those accessors as
  references to the field (`java.references.includeAccessors`), which is a
  setting, not a rule of the language.
- **document_highlight** follows jdtls's `Highlight.java`: besides the
  occurrences of a name, jdtls highlights where a thrown exception is
  thrown, where a method exits, the loop a `break` leaves and the methods
  implementing an interface.
- **implementation** asks every case of the hierarchy of jdtls's `hello`
  test project: `IFoo` is implemented by `Foo2`, which `Foo3` extends;
  `Foo` is extended by `FooChild`; nothing extends `AbstractFoo`.
- **rename** lists where each edit starts, on the line as it was before;
  the jdtls tests compare the sources after the rename. The new name is
  `renamed` (`Renamed` for a name that starts with a capital).
- **prepare_rename** and **rename** answer `(nothing)` where jdtls refuses
  with an error.
- **folding_range** uses the files of jdtls's `foldingRange` test project
  unchanged (tabs, blank lines and trailing spaces included: where a region
  ends depends on them). jdtls's tests count the regions of a file and name
  most of them; the few a test only counts are filled in by the same rules.
- **hover** is the signature line as jdtls words it, with the hovered name
  unqualified.
- **diagnostics** is the whole file's report (`textDocument/diagnostic`),
  written as `path:line:column  message`. javak-lsp reports a Java file's
  syntax errors and its missing `package` declaration in its own words, and
  in jdtls's words a feature the build file's source level does not have
  (a text block below 15) and a call no method of its name takes; a case
  either names those or expects `(nothing)`. A `(nothing)` case is the
  regression guard that a shape the parser handles stays unflagged. The
  missing-package check reads the package out of the path, so those cases
  live under `/src/main/java/`.
- A file whose header ends in `(not open)` is on disk and never opened, as
  is a build file: the server has to find it the way it finds the rest of a
  project. jdtls reads the whole project, not only the open files.
- **signature_help** cases that end in a syntax error do so on purpose, as
  the jdtls tests do: help is asked while a call is being typed.
- A case marked `//= expected (pending)` writes down what jdtls answers
  where javak-lsp has no such check yet: it passes while the two differ and
  fails the day the server answers it, so the marker comes off then.

## definition

jdtls: `NavigateToDefinitionHandlerTest`.

| case | jdtls test |
| --- | --- |
| `annotation` | — |
| `annotation_element` | — |
| `catch_parameter` | — |
| `constructor_call_is_the_constructor_with_those_arguments` | — |
| `declaration_is_itself` | — |
| `default_method_called_on_an_implementation` | — |
| `enum_constant` | — |
| `enum_constant_in_a_case_label` | — |
| `field_assigned_from_the_parameter_that_shadows_it` | — |
| `field_through_this_skips_the_local_of_the_same_name` | — |
| `fully_qualified_type` | — |
| `imported_type_in_its_import` | — |
| `inherited_method_is_in_the_superclass` | — |
| `lambda_parameter` | — |
| `last_field_of_a_field_chain` | — |
| `local_declared_with_var` | — |
| `local_variable` | — |
| `method_at_the_end_of_a_call_chain` | — |
| `method_of_a_type_in_a_file_no_editor_has_open` | — |
| `method_called_after_a_cast` | — |
| `method_called_on_an_array_element` | — |
| `method_linked_from_javadoc` | — |
| `method_on_the_type_argument_of_a_generic_receiver` | — |
| `method_reference` | — |
| `middle_field_of_a_field_chain` | — |
| `nested_class_named_through_its_outer_class` | — |
| `outer_field_read_from_an_inner_class` | — |
| `overload_is_chosen_by_the_argument_type` | — |
| `overridden_method_is_the_override_of_the_receivers_type` | — |
| `parameter_assigned_to_the_field_it_shadows` | — |
| `pattern_variable` | — |
| `prefers_the_method_of_the_anonymous_class_over_the_enclosing_one` | `testMethodInAnonymousClass2` |
| `record_accessor_is_the_record_component` | — |
| `single_type_import_shadows_the_type_of_the_same_package` | — |
| `static_field_read_through_its_class` | — |
| `static_method_called_on_its_class` | — |
| `statically_imported_field` | — |
| `super_call_is_the_method_of_the_superclass` | — |
| `type_follows_the_import_not_the_same_name_elsewhere` | — |
| `type_imported_on_demand_not_the_same_name_elsewhere` | — |
| `type_in_a_declaration` | — |
| `type_in_a_file_no_editor_has_open` | — |
| `type_parameter` | — |
| `variable_of_an_enhanced_for` | — |

## type_definition

jdtls: `NavigateToTypeDefinitionHandlerTest`.

| case | jdtls test |
| --- | --- |
| `enum_constant_is_the_enum` | — |
| `field_of_another_object` | `testExternalClassField` |
| `field_read_through_this` | `testClassField` |
| `generic_variable_is_the_generic_class` | — |
| `lambda_parameter_is_the_type_the_interface_gives_it` | — |
| `local_declared_with_var_is_the_inferred_class` | — |
| `local_is_the_class_it_is_declared_with` | `testLocalVariable` |
| `parameter` | — |
| `primitive_local_is_nothing` | — |
| `type_name_is_its_declaration` | — |

## implementation

jdtls: `ImplementationsHandlerTest`.

| case | jdtls test |
| --- | --- |
| `abstract_class_nothing_extends_is_the_class` | `testUnimplementedClassImplementation_includeDefinition` |
| `abstract_method_nothing_implements_is_the_method` | `testUnimplementedMethodImplementation_includeDefinition` |
| `call_on_a_class_includes_its_own_method` | `testMethodImplementation_includeDefinition` |
| `call_through_an_interface` | `testMethodInvocationImplementation` |
| `class_at_its_declaration_is_its_subclasses` | `testClassImplementation` |
| `class_where_it_is_used_includes_the_class` | `testClassImplementation_includeDefinition` |
| `interface_is_every_class_below_it` | `testInterfaceImplementation` |
| `interface_method_is_the_method_that_implements_it` | `testMethodImplementation` |
| `super_call_is_the_method_of_the_superclass` | `testMethodSuperInvocationImplementation` |
| `type_parameter_is_nothing` | `testInvalidElement` |

## references

jdtls: `ReferencesHandlerTest`.

| case | jdtls test |
| --- | --- |
| `class_everywhere_its_name_is_written` | — |
| `class_extended_in_another_file` | `testReference` |
| `class_nothing_uses_is_only_its_declaration` | `testPotentialMatch` (vscode-java#2227) |
| `class_used_in_a_file_no_editor_has_open` | — |
| `enum_constant_with_its_case_label` | — |
| `field_asked_from_a_use` | — |
| `field_read_and_written` | — |
| `interface_method_called_through_the_interface` | — |
| `method_across_files_skips_the_same_name_in_another_class` | — |
| `method_with_a_method_reference` | — |
| `method_with_its_declaration` | `testDeclarationInReferences` (eclipse.jdt.ls#2148) |
| `overload_skips_the_other_overloads` | — |
| `static_method_called_with_and_without_its_class` | — |

## document_highlight

jdtls: `DocumentHighlightHandlerTest`.

| case | jdtls test |
| --- | --- |
| `class_is_every_use_of_its_name_in_the_file` | — |
| `field_skips_the_local_of_the_same_name` | `testDocumentHighlight_Occurrences` |
| `implemented_interface_is_the_methods_implementing_it` | `testDocumentHighlight_ImplementOccurrences` |
| `labelled_break_is_the_loop_it_leaves` | `testDocumentHighlight_BreakContinueTarget` |
| `local_stays_inside_its_method` | — |
| `method_is_its_declaration_and_calls_in_the_file` | — |
| `return_is_where_the_method_exits` | `testDocumentHighlight_MethodExits` |
| `return_type_is_where_the_method_exits` | `testDocumentHighlight_MethodExits` |
| `thrown_exception_is_where_it_is_thrown` | `testDocumentHighlight_ExceptionOccurences` |

## completion

jdtls: `CompletionHandlerTest`.

| case | jdtls test |
| --- | --- |
| `annotation_element_by_prefix` | `testCompletion_annotationParam` |
| `enum_constant_in_a_case_label` | — |
| `field_of_the_outer_class_inside_an_inner_class` | — |
| `locals_and_parameters_declared_before_the_cursor` | — |
| `method_of_the_superclass_after_super` | — |
| `only_the_public_members_of_a_class_of_another_package` | — |

## hover

jdtls: `HoverHandlerTest`.

| case | jdtls test |
| --- | --- |
| `blank_line_is_nothing` | `testEmptyHover` |
| `class_is_its_name` | `testHover` |
| `constant_shows_its_value` | — |
| `constructor_call_shows_the_constructor` | — |
| `enum_constant_is_its_name` | `testEnum` |
| `field_of_a_generic_type_shows_its_type_arguments` | — |
| `field_shows_its_type` | — |
| `generic_class_shows_its_type_parameters` | — |
| `generic_method_shows_its_type_parameters` | `testHoverTypeParameters` |
| `inherited_method_called_on_a_subclass` | — |
| `interface_method_called_through_the_interface` | — |
| `local_names_the_method_it_belongs_to` | — |
| `method_returning_a_type_parameter` | `testHoverTypeParameters` |
| `method_shows_its_return_type_and_parameters` | `testHoverMethod` |
| `method_shows_the_exceptions_it_throws` | — |
| `method_with_array_and_generic_parameters` | — |
| `nested_type_is_its_own_name` | `testHoverOnJava11var` |
| `parameter_names_the_method_it_belongs_to` | `testHoverVariable` |
| `static_method_does_not_show_static` | `testHoverMethod` |
| `string_constant_shows_its_value_quoted` | — |
| `type_parameter_is_its_name` | `testHoverTypeParameters` |
| `varargs_method` | — |

## signature_help

jdtls: `SignatureHelpHandlerTest`.

| case | jdtls test |
| --- | --- |
| `after_a_cast_is_the_method_of_the_cast_type` | `testSignatureHelp_differentDeclaringType` |
| `after_a_nested_call_is_the_outer_method` | `testSignatureHelp_nestedInvocation` |
| `after_a_string_holding_a_parenthesis` | `testSignatureHelp_stringLiteral` |
| `after_a_trailing_comma_is_the_next_parameter` | `testSignatureHelp_parameters` (eclipse.jdt.ls#1015) |
| `before_a_constructor_argument_is_the_enclosing_call` | `testSignatureHelp_enclosingMethods` |
| `class_without_a_constructor_is_the_default_one` | `testSignatureHelp_defaultConstructor` |
| `generic_class_skips_the_constructor_it_cannot_call` | `testSignatureHelp_erasureType` |
| `in_a_call_that_is_not_closed` | `testSignatureHelp_double` (eclipse.jdt.ls#1980) |
| `in_the_first_call_of_a_chain` | `testSignatureHelp_methodChain` |
| `in_the_last_call_of_a_chain` | `testSignatureHelp_methodChain` |
| `inside_a_constructor_argument_is_the_constructor` | `testSignatureHelp_enclosingMethods` |
| `inside_a_nested_call_is_the_inner_method` | `testSignatureHelp_nestedInvocation` |
| `inside_a_string_holding_a_comma` | `testSignatureHelp_comma`, `testSignatureHelp_bracket` |
| `inside_a_string_holding_a_parenthesis` | `testSignatureHelp_stringLiteral` |
| `inside_the_first_array_argument` | `testSignatureHelp_complexArgs` |
| `inside_the_second_array_argument` | `testSignatureHelp_complexArgs` |
| `later_varargs_argument_is_still_the_varargs_parameter` | `testSignatureHelp_varargs3` |
| `on_the_method_name_marks_no_parameter` | `testSignatureHelp_callFromMethodName` |
| `outside_a_call_is_nothing` | `testSignatureHelp_invalid` |
| `picks_the_overload_by_the_number_of_arguments` | `testSignatureHelp_parameterTypes` |
| `picks_the_overload_by_the_type_of_the_argument` | `testSignatureHelp_parameterObject` |
| `picks_the_overload_the_arguments_already_fit` | `testSignatureHelp_activeSignature` |
| `picks_the_overload_with_room_for_the_argument_being_typed` | `testSignatureHelp_multipleMethods` |
| `record_is_its_canonical_constructor` | `testSignatureHelp_record` |
| `shows_the_type_arguments_of_the_receiver` | `testSignatureHelp_genericTypes` |
| `single_method` | `testSignatureHelp_singleMethod` |
| `super_call_is_the_constructor_of_the_superclass` | `testSignatureHelp_superConstructorInvocation` |
| `this_call_at_its_second_argument` | `testSignatureHelp_constructorInvocation` |
| `this_call_is_the_other_constructor` | `testSignatureHelp_constructorInvocation` |
| `varargs_argument_ignores_parentheses_in_a_string` | `testSignatureHelp_varargs4` |

## prepare_rename

jdtls: `PrepareRenameHandlerTest`.

| case | jdtls test |
| --- | --- |
| `call_of_an_implicit_record_accessor` | `testRenameRecordImplicitAccessorCall` |
| `class` | `testRenameTypeWithResourceChanges` |
| `compact_constructor_parameter` | `testRenameRecordCompactConstructorParameter` |
| `explicit_record_accessor` | `testRenameRecordExplicitAccessor` |
| `field` | `testRenameField` |
| `lambda_parameter` | `testRenameLambdaParameter` |
| `local_variable` | `testRenameLocalVariable` |
| `method` | `testRenameMethod` |
| `parameter` | `testRenameParameter` |
| `parameter_described_in_javadoc` | `testRenameJavadoc` |
| `refuses_a_class_of_the_jdk` | `testRenameClassFile` |
| `refuses_a_fully_qualified_class_of_the_jdk` | `testRenameFQCNClassFile` |
| `refuses_a_package_declaration` | `testRenamePackage` |
| `refuses_a_package_of_the_jdk` | `testRenameBinaryPackage` |
| `refuses_an_import` | `testRenameImportDeclaration` |
| `refuses_the_middle_of_a_package_declaration` | `testRenameMiddleOfPackage` |
| `type_parameter_of_a_class` | `testRenameTypeParameter` |
| `type_parameter_of_a_method` | `testRenameTypeParameterInMethod` |

## rename

jdtls: `RenameHandlerTest`.

| case | jdtls test |
| --- | --- |
| `at_super_renames_the_superclass` | `testRenameSuperMethod` |
| `class_renames_its_constructor` | `testRenameTypeWithResourceChanges` |
| `class_renames_its_uses_in_another_file` | `testRenameType` |
| `constructor_of_a_record_leaves_the_component_of_the_same_name` | `testRenameConstructorOfRecordWhoseComponentIsNamedLikeIt` |
| `constructor_renames_its_class` | `testRenameConstructor` |
| `field` | `testRenameField` |
| `from_a_call_of_an_implicit_accessor_renames_the_record_component` | `testRenameRecordComponentFromImplicitAccessorCall` |
| `from_a_canonical_constructor_parameter_renames_the_record_component` | `testRenameRecordComponentFromCanonicalConstructorParameter` |
| `from_a_compact_constructor_parameter_renames_the_record_component` | `testRenameRecordComponentFromCompactConstructorParameter` |
| `from_an_explicit_accessor_renames_the_record_component` | `testRenameRecordComponentFromExplicitAccessor` |
| `implementation_renames_the_interface_method` | `testRenameInterfaceMethod` |
| `local_variable_leaves_the_same_name_in_another_method` | `testRenameLocalVariable` |
| `method` | `testRenameMethod` |
| `method_from_a_call_in_another_file` | `testRenameMultipleFiles` |
| `method_parameter_named_like_a_record_component_leaves_the_component` | `testRenameParameterOfOrdinaryMethodNamedLikeRecordComponent` |
| `override_renames_every_method_tied_to_it_by_a_shared_subclass` | `testRenameOverrideMethodComplex` |
| `override_renames_the_method_it_overrides` | `testRenameOverrideMethodSimple` |
| `parameter_leaves_the_same_name_in_another_method` | `testRenameParameter` |
| `parameter_renames_its_javadoc_tag` | `testRenameJavadoc` |
| `record_component_renames_it_in_the_compact_constructor` | `testRenameRecordComponentWithCompactConstructorBody` |
| `record_component_renames_its_accessor_calls` | `testRenameRecordField` (vscode-java#2433) |
| `record_component_renames_its_explicit_accessor` | `testRenameRecordComponentWithExplicitAccessor` |
| `refuses_a_method_of_the_jdk` | `testRenameSystemLibrary` |
| `type_parameter_of_a_class` | `testRenameTypeParameter` |
| `type_parameter_of_a_method_leaves_the_one_of_the_class` | `testRenameTypeParameterInMethod` |

## document_symbol

jdtls: `DocumentSymbolHandlerTest`.

| case | jdtls test |
| --- | --- |
| `names_generic_declarations_without_their_type_parameters` | — |
| `outlines_an_enum_with_members` | `testDecompiledSource` |
| `outlines_an_interface` | — |
| `outlines_every_top_level_type_and_nesting_level` | — |
| `outlines_nested_types` | `testTypes`, `testTypes_hierarchical` |
| `tells_a_constant_from_a_field` | — |

## folding_range

jdtls: `FoldingRangeHandlerTest`.

| case | jdtls test |
| --- | --- |
| `file_that_does_not_scan` | `testInvalidInput` |
| `folds_comments_imports_and_bodies` | `testTypes` |
| `folds_nested_switches_and_braces_on_their_own_line` | `testNestedSwitchFoldingRanges`, `testCurlyBracesOwnLine` (eclipse.jdt.ls#2865) |
| `folds_region_comments` | `testRegionFoldingRanges` |
| `folds_statements` | `testStatementFoldingRanges` |
| `folds_static_initializers` | `testStaticBlockFoldingRange` |
| `unbalanced_braces` | `testErrorTypes` |

## workspace_symbol

jdtls: `WorkspaceSymbolHandlerTest`.

| case | jdtls test |
| --- | --- |
| `finds_interfaces_enums_and_nested_types` | — |
| `finds_the_same_name_in_two_packages` | `testProjectSearch` |
| `matches_a_lower_case_query_as_a_prefix` | `testSearchQualifiedTypeNoWildcards` |
| `matches_camel_case_parts_anywhere_in_the_name` | `testCamelCaseFuzzySearch` |
| `matches_capitals_as_camel_case` | `testCamelCaseSearch` |
| `nothing_for_a_name_no_type_has` | `testSearchWithEmptyResults` |

## diagnostics

jdtls: none — javak-lsp's own checks (syntax errors, missing `package`), and
two of jdtls's worded as jdtls words them (source level, calls).

| case | what it asks |
| --- | --- |
| `a_body_that_is_never_closed` | the unclosed brace of a method body |
| `a_brace_with_nothing_left_to_close` | a `}` past the end of the class |
| `a_call_whose_parenthesis_is_never_closed` | the `)` a call never gets |
| `a_call_missing_an_argument_is_reported` | the call `parse(test)` has no applicable overload of `parse(String, Parser)` |
| `a_call_of_a_method_an_unknown_superclass_may_declare_is_not_reported` | nothing: the superclass may have the overload |
| `a_call_of_an_overload_that_takes_its_arguments_is_not_reported` | nothing: another overload, or varargs, takes them |
| `a_call_with_too_many_arguments_is_reported` | `this.count(test, 1)` of `count()` |
| `a_class_declared_without_a_body` | a class header with no `{}` after it |
| `a_deeply_nested_package_is_named_in_full` | `com.demo.data.local`, every segment of the path |
| `a_directory_that_is_not_an_identifier_names_no_package` | nothing: `my-demo` is no package segment |
| `a_field_without_its_semicolon` | the missing `;` of a field |
| `a_file_directly_at_its_source_root_has_no_package` | nothing: no package to name |
| `a_file_that_declares_its_package` | nothing |
| `a_file_under_no_source_root_is_left_alone` | nothing: the path says no package |
| `a_file_whose_package_is_not_declared` | the missing `package com.demo` |
| `a_generic_method_with_bounds_and_wildcards_parses` | nothing |
| `a_labelled_break_and_continue_parse` | nothing |
| `a_lambda_with_a_block_body_and_a_method_reference_parse` | nothing |
| `a_method_with_throws_synchronized_and_a_do_while_parses` | nothing |
| `a_missing_package_and_a_syntax_error_are_both_reported` | both, in source order |
| `a_nested_class_an_inner_class_and_a_local_class_parse` | nothing |
| `a_package_declared_after_a_licence_header_is_a_package` | nothing: a comment above it is not a declaration |
| `a_package_declared_after_an_import_is_still_a_package` | nothing |
| `a_record_with_a_compact_constructor_and_a_static_member_parses` | nothing |
| `a_statement_without_its_semicolon` | the missing `;` |
| `a_string_that_is_never_closed` | what the parser makes of the rest of the file |
| `a_switch_statement_with_fall_through_and_a_default_parses` | nothing |
| `a_test_source_root_is_read_the_same_way` | the missing package of a `src/test/java` file |
| `a_text_block_containing_quotes_and_braces_parses` | nothing: `"""` is not an unclosed string |
| `a_text_block_at_source_level_15_is_not_reported` | nothing: `pom.xml` says 17 |
| `a_text_block_under_source_level_15_is_reported` | a text block in a project whose `pom.xml` says source level 11 |
| `a_type_annotated_generic_field_parses` | nothing: nested generics and a `? super` bound |
| `a_unicode_identifier_parses` | nothing: `café` is a name |
| `an_annotation_declaration_with_elements_parses` | nothing: `@interface` with `default` values |
| `an_anonymous_class_parses` | nothing |
| `an_arrow_switch_with_a_yielding_block_parses` | nothing: `yield` from a block arm |
| `an_empty_file_is_only_its_missing_package` | the missing `package`, nothing else |
| `an_enum_with_a_constructor_fields_and_a_constant_body_parses` | nothing: a constant with a body of its own |
| `an_initializer_with_no_expression` | `int x = ;` |
| `an_instanceof_pattern_and_a_ternary_parse` | nothing |
| `an_interface_with_default_static_and_private_methods_parses` | nothing |
| `annotated_members_parse` | nothing: `@Override`/`@SafeVarargs` are not errors |
| `braces_and_quotes_inside_comments_are_not_code` | nothing: an unbalanced `{` in a comment is text |
| `each_statement_without_a_semicolon_is_its_own_error` | one error per statement |
| `numeric_character_and_escape_literals_parse` | nothing: hex, binary, `_` groups, escapes |
| `records_sealed_types_switch_patterns_and_text_blocks_parse` | nothing: modern Java is not a syntax error |
| `static_and_instance_initializer_blocks_parse` | nothing |
| `try_with_resources_and_a_multi_catch_parse` | nothing |
| `varargs_an_array_initializer_and_a_cast_parse` | nothing |

## Ported from jdtls: answers still to write

Every case below was generated from the jdtls handler test it names
(eclipse-jdtls/eclipse.jdt.ls at `5eac545`): the test's sources, its
cursor and, where it selects text, the selection `$0`…`$1`. Sources that come
from one of jdtls's test projects (`eclipse/hello`, `maven/salut`, …) keep
their path in that project; the other files of a small project come along
`(not open)`.

Their `//= expected` block is empty: each one fails with
`(nothing written down yet)` until the answer jdtls gives is written in by
hand (or recorded with `compat`, then checked). A jdtls test that asks
several questions is one case per question, numbered `_1`, `_2`, ….

The answer of some of these is more than the rendering shows: a hover is its
signature line, not its Javadoc; a completion is the names offered, not the
text a snippet inserts. Those cases still ask the question at the jdtls
position; what the rendering leaves out is not checked.

### completion

| case | jdtls test |
| --- | --- |
| `additional_text_edit` | `CompletionHandlerTest#testCompletion_additionalTextEdit` |
| `annotation_boolean_attribute_value` | `CompletionHandlerTest#testCompletion_annotationBooleanAttributeValue` |
| `annotation_boolean_attribute_value_filters_methods_and_packages` | `CompletionHandlerTest#testCompletion_annotationBooleanAttributeValue_filtersMethodsAndPackages` |
| `annotation_boolean_attribute_value_qualified_name_no_boolean_injection` | `CompletionHandlerTest#testCompletion_annotationBooleanAttributeValue_qualifiedNameNoBooleanInjection` |
| `annotation_string_attribute_value_no_boolean_injection` | `CompletionHandlerTest#testCompletion_annotationStringAttributeValue_noBooleanInjection` |
| `annotation_trailing_space` | `CompletionHandlerTest#testCompletion_annotationTrailingSpace` |
| `annotation_with_arguments_no_space_before_parenthesis` | `CompletionHandlerTest#testCompletion_annotationWithArgumentsNoSpaceBeforeParenthesis` |
| `anonymous` | `CompletionHandlerTest#testCompletion_Anonymous` |
| `anonymous_declaration_type` | `CompletionHandlerTest#testCompletion_AnonymousDeclarationType` |
| `anonymous_declaration_type2` | `CompletionHandlerTest#testCompletion_AnonymousDeclarationType2` |
| `anonymous_declaration_type3` | `CompletionHandlerTest#testCompletion_AnonymousDeclarationType3` |
| `anonymous_declaration_type4` | `CompletionHandlerTest#testCompletion_AnonymousDeclarationType4` |
| `anonymous_declaration_type5` | `CompletionHandlerTest#testCompletion_AnonymousDeclarationType5` |
| `anonymous_declaration_type_no_snippet` | `CompletionHandlerTest#testCompletion_AnonymousDeclarationType_noSnippet` |
| `anonymous_type` | `CompletionHandlerTest#testCompletion_AnonymousType` |
| `anonymous_type_more_methods` | `CompletionHandlerTest#testCompletion_AnonymousTypeMoreMethods` |
| `auto_add_static_import_as_favorite_import` | `CompletionHandlerTest#testCompletion_autoAddStaticImportAsFavoriteImport` |
| `booleangetter` | `CompletionHandlerTest#testCompletion_booleangetter` |
| `chain_completions_on_chains_correct_snippet_placeholders` | `CompletionHandlerChainTest#testChainCompletionsOnChainsCorrectSnippetPlaceholders` |
| `chain_completions_on_chains_from_visible_variables` | `CompletionHandlerChainTest#testChainCompletionsOnChainsFromVisibleVariables` |
| `chain_completions_on_object_variable_expect_no_completions_1` | `CompletionHandlerChainTest#testChainCompletionsOnObjectVariableExpectNoCompletions` |
| `chain_completions_on_object_variable_expect_no_completions_2` | `CompletionHandlerChainTest#testChainCompletionsOnObjectVariableExpectNoCompletions` |
| `chain_completions_on_parameter` | `CompletionHandlerChainTest#testChainCompletionsOnParameter` |
| `chain_completions_on_primitive_variable_expect_no_completions` | `CompletionHandlerChainTest#testChainCompletionsOnPrimitiveVariableExpectNoCompletions` |
| `chain_completions_on_string_variable_expect_no_completions_1` | `CompletionHandlerChainTest#testChainCompletionsOnStringVariableExpectNoCompletions` |
| `chain_completions_on_string_variable_expect_no_completions_2` | `CompletionHandlerChainTest#testChainCompletionsOnStringVariableExpectNoCompletions` |
| `chain_completions_on_variable` | `CompletionHandlerChainTest#testChainCompletionsOnVariable` |
| `chain_completions_on_variable_completing_constructor_expect_no_chains` | `CompletionHandlerChainTest#testChainCompletionsOnVariableCompletingConstructorExpectNoChains` |
| `chain_completions_on_variable_with_new_keyword_expect_no_chains` | `CompletionHandlerChainTest#testChainCompletionsOnVariableWithNewKeywordExpectNoChains` |
| `chain_completions_on_variable_with_token_matching_edge` | `CompletionHandlerChainTest#testChainCompletionsOnVariableWithTokenMatchingEdge` |
| `chain_completions_on_variable_with_token_matching_start` | `CompletionHandlerChainTest#testChainCompletionsOnVariableWithTokenMatchingStart` |
| `chain_completions_with_token_expect_replace_token_1` | `CompletionHandlerChainTest#testChainCompletionsWithToken_ExpectReplaceToken` |
| `chain_completions_with_token_expect_replace_token_2` | `CompletionHandlerChainTest#testChainCompletionsWithToken_ExpectReplaceToken` |
| `class_method_override_java4` | `CompletionHandlerTest#testCompletion_classMethodOverrideJava4` |
| `class_method_override_java5` | `CompletionHandlerTest#testCompletion_classMethodOverrideJava5` |
| `class_method_override_no_snippet` | `CompletionHandlerTest#testCompletion_classMethodOverrideNoSnippet` |
| `collapse` | `CompletionHandlerTest#testCompletion_collapse` |
| `collapse_extends` | `CompletionHandlerTest#testCompletion_collapse_extends` |
| `constant_default_value_1` | `CompletionHandlerTest#testCompletion_ConstantDefaultValue` |
| `constant_default_value_2` | `CompletionHandlerTest#testCompletion_ConstantDefaultValue` |
| `constructor` | `CompletionHandlerTest#testCompletion_constructor` |
| `constructor_turn_off_guess_method_arguments` | `CompletionHandlerTest#testCompletion_constructor_turnOffGuessMethodArguments` |
| `data_field_execution_time` | `CompletionHandlerTest#testCompletion_dataFieldExecutionTime` |
| `data_field_uri` | `CompletionHandlerTest#testCompletion_dataFieldURI` |
| `deprecated_property` | `CompletionHandlerTest#testCompletion_Deprecated_property` |
| `field_item_defaults_enabled` | `CompletionHandlerTest#testCompletion_field_itemDefaults_enabled` |
| `field_item_defaults_enabled_adjust_indentation` | `CompletionHandlerTest#testCompletion_field_itemDefaults_enabled_AdjustIndentation` |
| `filter_methods_when_type_is_missing` | `CompletionHandlerTest#testCompletion_FilterMethodsWhenTypeIsMissing` |
| `filter_packages` | `CompletionHandlerTest#testCompletion_FilterPackages` |
| `filter_types_1` | `CompletionHandlerTest#testCompletion_FilterTypes` |
| `filter_types_2` | `CompletionHandlerTest#testCompletion_FilterTypes` |
| `filter_types_keep_methods` | `CompletionHandlerTest#testCompletion_FilterTypesKeepMethods` |
| `filter_types_keep_methods2` | `CompletionHandlerTest#testCompletion_FilterTypesKeepMethods2` |
| `for_enclosing_type_array_type_receivers` | `CompletionHandlerTest#testCompletion_forEnclosingTypeArrayTypeReceivers` |
| `getter` | `CompletionHandlerTest#testCompletion_getter` |
| `getter_no_javadoc` | `CompletionHandlerTest#testCompletion_getterNoJavadoc` |
| `ignore_space_without_new` | `CompletionHandlerTest#testCompletion_IgnoreSpaceWithoutNew` |
| `ignore_string_literal_new` | `CompletionHandlerTest#testCompletion_IgnoreStringLiteralNew` |
| `ignore_type_filter_when_imported1` | `CompletionHandlerTest#testCompletion_IgnoreTypeFilterWhenImported1` |
| `ignore_type_filter_when_imported2` | `CompletionHandlerTest#testCompletion_IgnoreTypeFilterWhenImported2` |
| `ignore_type_filter_when_imported3` | `CompletionHandlerTest#testCompletion_IgnoreTypeFilterWhenImported3` |
| `ignore_type_filter_when_imported4` | `CompletionHandlerTest#testCompletion_IgnoreTypeFilterWhenImported4` |
| `ignore_type_filter_when_imported5` | `CompletionHandlerTest#testCompletion_IgnoreTypeFilterWhenImported5` |
| `ignore_type_filter_when_imported6` | `CompletionHandlerTest#testCompletion_IgnoreTypeFilterWhenImported6` |
| `ignore_variable_with_new_postfix` | `CompletionHandlerTest#testCompletion_IgnoreVariableWithNewPostfix` |
| `import_static` | `CompletionHandlerTest#testCompletion_import_static` |
| `insert` | `CompletionHandlerTest#testCompletion_insert` |
| `insert_replace_edit` | `CompletionInsertReplaceCapabilityTest#testCompletion_InsertReplaceEdit` |
| `javadoc` | `CompletionHandlerTest#testCompletion_javadoc` |
| `javadoc_comment` | `CompletionHandlerTest#testCompletion_javadocComment` |
| `javadoc_comment_no_param` | `CompletionHandlerTest#testCompletion_javadocCommentNoParam` |
| `javadoc_comment_no_snippet` | `CompletionHandlerTest#testCompletion_javadocCommentNoSnippet` |
| `javadoc_comment_partial` | `CompletionHandlerTest#testCompletion_javadocCommentPartial` |
| `javadoc_comment_record` | `CompletionHandlerTest#testCompletion_javadocCommentRecord` |
| `javadoc_comment_record_no_snippet` | `CompletionHandlerTest#testCompletion_javadocCommentRecordNoSnippet` |
| `javadoc_comment_regular` | `CompletionHandlerTest#testCompletion_javadocCommentRegular` |
| `javadoc_markdown` | `CompletionHandlerTest#testCompletion_javadocMarkdown` |
| `javadoc_original` | `CompletionHandlerTest#testCompletion_javadocOriginal` |
| `keyword_trailing_space` | `CompletionHandlerTest#testCompletion_keywordTrailingSpace` |
| `lambda` | `CompletionHandlerTest#testCompletion_Lambda` |
| `lambda_with_multiple_params` | `CompletionHandlerTest#testCompletion_lambdaWithMultipleParams` |
| `lambda_with_no_param` | `CompletionHandlerTest#testCompletion_lambdaWithNoParam` |
| `lazy_resolve_constructor_completion` | `CompletionHandlerLazyResolveTest#testConstructorCompletion` |
| `lazy_resolve_fully_qualified_type_completion1` | `CompletionHandlerLazyResolveTest#testFullyQualifiedTypeCompletion1` |
| `lazy_resolve_fully_qualified_type_completion2` | `CompletionHandlerLazyResolveTest#testFullyQualifiedTypeCompletion2` |
| `lazy_resolve_snippet_array_foreach` | `CompletionHandlerLazyResolveTest#testSnippet_array_foreach` |
| `lazy_resolve_snippet_array_fori` | `CompletionHandlerLazyResolveTest#testSnippet_array_fori` |
| `lazy_resolve_snippet_dowhile` | `CompletionHandlerLazyResolveTest#testSnippet_dowhile` |
| `lazy_resolve_snippet_if` | `CompletionHandlerLazyResolveTest#testSnippet_if` |
| `lazy_resolve_snippet_ifelse` | `CompletionHandlerLazyResolveTest#testSnippet_ifelse` |
| `lazy_resolve_snippet_ifnotnull` | `CompletionHandlerLazyResolveTest#testSnippet_ifnotnull` |
| `lazy_resolve_snippet_ifnull` | `CompletionHandlerLazyResolveTest#testSnippet_ifnull` |
| `lazy_resolve_snippet_list_foreach` | `CompletionHandlerLazyResolveTest#testSnippet_list_foreach` |
| `lazy_resolve_snippet_list_iter` | `CompletionHandlerLazyResolveTest#testSnippet_list_iter` |
| `lazy_resolve_snippet_serr` | `CompletionHandlerLazyResolveTest#testSnippet_serr` |
| `lazy_resolve_snippet_sout` | `CompletionHandlerLazyResolveTest#testSnippet_sout` |
| `lazy_resolve_snippet_soutm` | `CompletionHandlerLazyResolveTest#testSnippet_soutm` |
| `lazy_resolve_snippet_syserr` | `CompletionHandlerLazyResolveTest#testSnippet_syserr` |
| `lazy_resolve_snippet_sysout` | `CompletionHandlerLazyResolveTest#testSnippet_sysout` |
| `lazy_resolve_snippet_systrace` | `CompletionHandlerLazyResolveTest#testSnippet_systrace` |
| `lazy_resolve_snippet_while` | `CompletionHandlerLazyResolveTest#testSnippet_while` |
| `lazy_resolve_snippet_while_item_defaults_enabled_generic_snippets` | `CompletionHandlerLazyResolveTest#testSnippet_while_itemDefaults_enabled_generic_snippets` |
| `limit_completion_results_1` | `CompletionHandlerTest#testLimitCompletionResults` |
| `limit_completion_results_2` | `CompletionHandlerTest#testLimitCompletionResults` |
| `limit_completion_results_3` | `CompletionHandlerTest#testLimitCompletionResults` |
| `links_in_markdown` | `CompletionHandlerTest#testCompletion_linksInMarkdown` |
| `match_case_first_letter` | `CompletionHandlerTest#testCompletion_MatchCaseFirstLetter` |
| `match_case_first_letter_for_constructor` | `CompletionHandlerTest#testCompletion_MatchCaseFirstLetterForConstructor` |
| `match_case_first_letter_for_method_override` | `CompletionHandlerTest#testCompletion_MatchCaseFirstLetterForMethodOverride` |
| `match_case_off` | `CompletionHandlerTest#testCompletion_MatchCaseOff` |
| `method_guess_method_arguments2` | `CompletionHandlerTest#testCompletion_method_guessMethodArguments2` |
| `method_guess_method_arguments3` | `CompletionHandlerTest#testCompletion_method_guessMethodArguments3` |
| `method_guess_method_arguments_constructor` | `CompletionHandlerTest#testCompletion_method_guessMethodArgumentsConstructor` |
| `method_insert_best_guessed_arguments` | `CompletionHandlerTest#testCompletion_method_insertBestGuessedArguments` |
| `method_insert_parameter_names` | `CompletionHandlerTest#testCompletion_method_insertParameterNames` |
| `method_override` | `CompletionHandlerTest#testCompletion_methodOverride` |
| `method_override_with_exception` | `CompletionHandlerTest#testCompletion_methodOverrideWithException` |
| `method_override_with_params` | `CompletionHandlerTest#testCompletion_methodOverrideWithParams` |
| `method_turn_off_guess_method_arguments` | `CompletionHandlerTest#testCompletion_method_turnOffGuessMethodArguments` |
| `method_with_lspv2` | `CompletionHandlerTest#testCompletion_method_withLSPV2` |
| `method_with_lspv3` | `CompletionHandlerTest#testCompletion_method_withLSPV3` |
| `module_completion_resolve_shows_documentation` | `CompletionResolveHandlerTest#testModuleCompletion_resolve_showsDocumentation` |
| `multi_line_range` | `CompletionHandlerTest#testCompletion_multiLineRange` |
| `no_package` | `CompletionHandlerTest#testCompletion_noPackage` |
| `nojavadoc` | `CompletionHandlerTest#testCompletion_nojavadoc` |
| `nullable` | `CompletionHandlerTest#testCompletion_Nullable` |
| `object` | `CompletionHandlerTest#testCompletion_object` |
| `order` | `CompletionHandlerTest#testCompletion_order` |
| `overwrite` | `CompletionHandlerTest#testCompletion_overwrite` |
| `postfix_assert` | `PostfixCompletionTest#test_assert` |
| `postfix_can_evaluate` | `PostfixCompletionTest#test_canEvaluate` |
| `postfix_can_evaluate2` | `PostfixCompletionTest#test_canEvaluate2` |
| `postfix_cast` | `PostfixCompletionTest#test_cast` |
| `postfix_cast_lazy_resolve` | `PostfixCompletionTest#testCastLazyResolve` |
| `postfix_completion_generic_anonymous_class` | `PostfixCompletionTest#testCompletion_GenericAnonymousClass` |
| `postfix_else` | `PostfixCompletionTest#test_else` |
| `postfix_for` | `PostfixCompletionTest#test_for` |
| `postfix_fori` | `PostfixCompletionTest#test_fori` |
| `postfix_format` | `PostfixCompletionTest#test_format` |
| `postfix_forr` | `PostfixCompletionTest#test_forr` |
| `postfix_if` | `PostfixCompletionTest#test_if` |
| `postfix_java_doc` | `PostfixCompletionTest#test_JavaDoc` |
| `postfix_nnull` | `PostfixCompletionTest#test_nnull` |
| `postfix_not` | `PostfixCompletionTest#test_not` |
| `postfix_null` | `PostfixCompletionTest#test_null` |
| `postfix_opt` | `PostfixCompletionTest#test_opt` |
| `postfix_par` | `PostfixCompletionTest#test_par` |
| `postfix_postfix_completion_not_in_import_declaration` | `PostfixCompletionTest#testPostfixCompletion_NotInImportDeclaration` |
| `postfix_syserr_object` | `PostfixCompletionTest#test_syserr_object` |
| `postfix_sysouf_object` | `PostfixCompletionTest#test_sysouf_object` |
| `postfix_sysout` | `PostfixCompletionTest#test_sysout` |
| `postfix_sysout_item_defaults_enabled` | `PostfixCompletionTest#test_sysout_itemDefaults_enabled` |
| `postfix_sysout_object` | `PostfixCompletionTest#test_sysout_object` |
| `postfix_sysoutv_object` | `PostfixCompletionTest#test_sysoutv_object` |
| `postfix_throw` | `PostfixCompletionTest#test_throw` |
| `postfix_var` | `PostfixCompletionTest#test_var` |
| `postfix_var2` | `PostfixCompletionTest#test_var2` |
| `postfix_var_filter_text_spans_untyped_key` | `PostfixCompletionTest#test_var_filterTextSpansUntypedKey` |
| `postfix_while` | `PostfixCompletionTest#test_while` |
| `print_snippets` | `CompletionHandlerTest#testCompletion_printSnippets` |
| `publicmain_snippet` | `CompletionHandlerTest#testCompletion_publicmainSnippet` |
| `qualified_name2` | `CompletionHandlerTest#testCompletion_QualifiedName2` |
| `resolve_additional_text_edits` | `CompletionHandlerTest#testCompletion_resolveAdditionalTextEdits` |
| `select_snippet_item` | `CompletionHandlerTest#testCompletion_selectSnippetItem` |
| `setter` | `CompletionHandlerTest#testCompletion_setter` |
| `skip_additional_edit_for_import` | `CompletionHandlerTest#testSkipAdditionalEditForImport` |
| `skip_additional_edit_for_import2` | `CompletionHandlerTest#testSkipAdditionalEditForImport2` |
| `snippet_class` | `CompletionHandlerTest#testSnippet_class` |
| `snippet_class_no_static_method` | `CompletionHandlerTest#testSnippet_class_no_static_method` |
| `snippet_class_with_package` | `CompletionHandlerTest#testSnippet_class_with_package` |
| `snippet_class_with_package_package_private` | `CompletionHandlerTest#testSnippet_class_with_package_packagePrivate` |
| `snippet_context_javadoc` | `CompletionHandlerTest#testSnippet_context_javadoc` |
| `snippet_context_method1` | `CompletionHandlerTest#testSnippet_context_method1` |
| `snippet_context_method2` | `CompletionHandlerTest#testSnippet_context_method2` |
| `snippet_context_method3` | `CompletionHandlerTest#testSnippet_context_method3` |
| `snippet_context_package` | `CompletionHandlerTest#testSnippet_context_package` |
| `snippet_context_static` | `CompletionHandlerTest#testSnippet_context_static` |
| `snippet_ctor` | `CompletionHandlerTest#testSnippet_ctor` |
| `snippet_enum` | `CompletionHandlerTest#testSnippet_enum` |
| `snippet_enum_with_package` | `CompletionHandlerTest#testSnippet_enum_with_package` |
| `snippet_enum_with_package_package_private` | `CompletionHandlerTest#testSnippet_enum_with_package_packagePrivate` |
| `snippet_if2` | `CompletionHandlerTest#testSnippet_if2` |
| `snippet_ifelse2` | `CompletionHandlerTest#testSnippet_ifelse2` |
| `snippet_inner_class` | `CompletionHandlerTest#testSnippet_inner_class` |
| `snippet_inner_class_item_defaults_enabled_type_definition` | `CompletionHandlerTest#testSnippet_inner_class_itemDefaults_enabled_type_definition` |
| `snippet_inner_interface` | `CompletionHandlerTest#testSnippet_inner_interface` |
| `snippet_inner_record` | `CompletionHandlerTest#testSnippet_inner_record` |
| `snippet_instance_main` | `CompletionHandlerTest#testSnippet_instance_main` |
| `snippet_interface` | `CompletionHandlerTest#testSnippet_interface` |
| `snippet_interface_method` | `CompletionHandlerTest#testSnippet_interface_method` |
| `snippet_interface_no_ctor` | `CompletionHandlerTest#testSnippet_interface_no_ctor` |
| `snippet_interface_with_package` | `CompletionHandlerTest#testSnippet_interface_with_package` |
| `snippet_interface_with_package_package_private` | `CompletionHandlerTest#testSnippet_interface_with_package_packagePrivate` |
| `snippet_io_statements` | `CompletionHandlerTest#testSnippet_io_statements` |
| `snippet_multiline_string` | `CompletionHandlerTest#testSnippet_multiline_string` |
| `snippet_nested_inner_class` | `CompletionHandlerTest#testSnippet_nested_inner_class` |
| `snippet_nested_inner_interface` | `CompletionHandlerTest#testSnippet_nested_inner_interface` |
| `snippet_nested_inner_interface_nosnippet` | `CompletionHandlerTest#testSnippet_nested_inner_interface_nosnippet` |
| `snippet_nested_inner_record` | `CompletionHandlerTest#testSnippet_nested_inner_record` |
| `snippet_nested_inner_record_nosnippet` | `CompletionHandlerTest#testSnippet_nested_inner_record_nosnippet` |
| `snippet_no_record` | `CompletionHandlerTest#testSnippet_no_record` |
| `snippet_non_lazy_resolve` | `CompletionHandlerTest#testSnippet_NonLazyResolve` |
| `snippet_record` | `CompletionHandlerTest#testSnippet_record` |
| `snippet_record_with_package` | `CompletionHandlerTest#testSnippet_record_with_package` |
| `snippet_record_with_package_package_private` | `CompletionHandlerTest#testSnippet_record_with_package_packagePrivate` |
| `snippet_sibling_inner_class` | `CompletionHandlerTest#testSnippet_sibling_inner_class` |
| `snippet_sibling_inner_class_nosnippets` | `CompletionHandlerTest#testSnippet_sibling_inner_class_nosnippets` |
| `snippet_sibling_inner_interface` | `CompletionHandlerTest#testSnippet_sibling_inner_interface` |
| `snippet_sibling_inner_record` | `CompletionHandlerTest#testSnippet_sibling_inner_record` |
| `snippet_while_item_defaults_enabled_generic_snippets` | `CompletionResolveHandlerTest#testSnippet_while_itemDefaults_enabled_generic_snippets` |
| `snippet_with_public` | `CompletionHandlerTest#testSnippet_with_public` |
| `star_imports_1` | `CompletionHandlerTest#testStarImports` |
| `star_imports_2` | `CompletionHandlerTest#testStarImports` |
| `static_imports1` | `CompletionHandlerTest#testStaticImports1` |
| `static_imports2` | `CompletionHandlerTest#testStaticImports2` |
| `syserr_snipper` | `CompletionHandlerTest#testCompletion_syserrSnipper` |
| `test_classes_available_into_test_code` | `CompletionHandlerTest#testCompletion_testClassesAvailableIntoTestCode` |
| `test_classes_dont_leak_into_main_code` | `CompletionHandlerTest#testCompletion_testClassesDontLeakIntoMainCode` |
| `test_method_with_params` | `CompletionHandlerTest#testCompletion_testMethodWithParams` |
| `type` | `CompletionHandlerTest#testCompletion_type` |

### hover

| case | jdtls test |
| --- | --- |
| `implicitly_inherited_tags_in_markdown_comment` | `HoverHandlerTest#testHoverImplicitlyInheritedTagsInMarkdownComment` |
| `incorrect_javadoc_rendering_3649` | `HoverHandlerTest#testIncorrectJavadocRendering_3649` |
| `incorrect_javadoc_rendering_see_and_custom_tag` | `HoverHandlerTest#testIncorrectJavadocRendering_seeAndCustomTag` |
| `inherit_doc_in_markdown_comment` | `HoverHandlerTest#testHoverInheritDocInMarkdownComment` |
| `inherited_javadoc` | `HoverHandlerTest#testHoverInheritedJavadoc` |
| `inline_code_tag_in_markdown_comment` | `HoverHandlerTest#testHoverInlineCodeTagInMarkdownComment` |
| `inline_link_tag_in_markdown_01` | `HoverHandlerTest#testHoverInlineLinkTagInMarkdown_01` |
| `inline_link_tag_in_markdown_02` | `HoverHandlerTest#testHoverInlineLinkTagInMarkdown_02` |
| `inline_literal_tag_in_markdown_comment` | `HoverHandlerTest#testHoverInlineLiteralTagInMarkdownComment` |
| `javadoc_disabled` | `HoverHandlerTest#testHoverJavadocDisabled` |
| `javadoc_dl_dt_dd` | `HoverHandlerTest#testHoverJavadocDlDtDd` |
| `javadoc_link_plain` | `HoverHandlerTest#testHoverJavadocLinkPlain` |
| `javadoc_snippet` | `HoverHandlerTest#testHoverJavadocSnippet` |
| `javadoc_snippet2` | `HoverHandlerTest#testHoverJavadocSnippet2` |
| `javadoc_with_extra_tags` | `HoverHandlerTest#testHoverJavadocWithExtraTags` |
| `javadoc_with_index_tag` | `HoverHandlerTest#testHoverJavadocWithIndexTag` |
| `markdown_comment` | `HoverHandlerTest#testHoverMarkdownComment` |
| `multiline_code_tag_in_markdown_comment` | `HoverHandlerTest#testHoverMultilineCodeTagInMarkdownComment` |
| `multiline_literal_tag_in_markdown_comment` | `HoverHandlerTest#testHoverMultilineLiteralTagInMarkdownComment` |
| `on_java10var` | `HoverHandlerTest#testHoverOnJava10var` |
| `on_javadoc_with_link_to_method_in_class` | `HoverHandlerTest#testHoverOnJavadocWithLinkToMethodInClass` |
| `on_javadoc_with_link_to_method_in_other_class` | `HoverHandlerTest#testHoverOnJavadocWithLinkToMethodInOtherClass` |
| `on_javadoc_with_multiple_different_types_of_tags` | `HoverHandlerTest#testHoverOnJavadocWithMultipleDifferentTypesOfTags` |
| `on_javadoc_with_value_tag` | `HoverHandlerTest#testHoverOnJavadocWithValueTag` |
| `on_package_with_javadoc` | `HoverHandlerTest#testHoverOnPackageWithJavadoc` |
| `on_package_with_new_javadoc` | `HoverHandlerTest#testHoverOnPackageWithNewJavadoc` |
| `over_null_element` | `HoverHandlerTest#testHoverOverNullElement` |
| `package` | `HoverHandlerTest#testHoverPackage` |
| `plain_markdown_method_preserves_formatting` | `HoverHandlerTest#testHoverPlainMarkdownMethodPreservesFormatting` |
| `resolved_method_and_constructor_links_in_markdown` | `HoverHandlerTest#testHoverResolvedMethodAndConstructorLinksInMarkdown` |
| `unresolved_type` | `HoverHandlerTest#testHoverUnresolvedType` |
| `value_tag_in_markdown_comment` | `HoverHandlerTest#testHoverValueTagInMarkdownComment` |
| `when_link_does_not_exist` | `HoverHandlerTest#testHoverWhenLinkDoesNotExist` |
| `with_attached_javadoc` | `HoverHandlerTest#testHoverWithAttachedJavadoc` |

### signature_help

| case | jdtls test |
| --- | --- |
| `assert_equals_1` | `SignatureHelpHandlerTest#testSignatureHelp_assertEquals` |
| `assert_equals_2` | `SignatureHelpHandlerTest#testSignatureHelp_assertEquals` |
| `assert_equals_3` | `SignatureHelpHandlerTest#testSignatureHelp_assertEquals` |
| `assert_equals_4` | `SignatureHelpHandlerTest#testSignatureHelp_assertEquals` |
| `assert_equals_5` | `SignatureHelpHandlerTest#testSignatureHelp_assertEquals` |
| `assert_equals_6` | `SignatureHelpHandlerTest#testSignatureHelp_assertEquals` |
| `assert_equals_7` | `SignatureHelpHandlerTest#testSignatureHelp_assertEquals` |
| `binary_1` | `SignatureHelpHandlerTest#testSignatureHelp_binary` |
| `binary_2` | `SignatureHelpHandlerTest#testSignatureHelp_binary` |
| `bracket2` | `SignatureHelpHandlerTest#testSignatureHelp_bracket2` |
| `bracket3` | `SignatureHelpHandlerTest#testSignatureHelp_bracket3` |
| `bracket4` | `SignatureHelpHandlerTest#testSignatureHelp_bracket4` |
| `constructor` | `SignatureHelpHandlerTest#testSignatureHelp_constructor` |
| `constructor2_1` | `SignatureHelpHandlerTest#testSignatureHelp_constructor2` |
| `constructor2_2` | `SignatureHelpHandlerTest#testSignatureHelp_constructor2` |
| `constructor_1` | `SignatureHelpHandlerTest#testSignatureHelpConstructor` |
| `constructor_2` | `SignatureHelpHandlerTest#testSignatureHelpConstructor` |
| `constructor_parameters` | `SignatureHelpHandlerTest#testSignatureHelp_constructorParameters` |
| `constructor_parameters2` | `SignatureHelpHandlerTest#testSignatureHelp_constructorParameters2` |
| `constructor_parameters3` | `SignatureHelpHandlerTest#testSignatureHelp_constructorParameters3` |
| `description_disabled` | `SignatureHelpHandlerTest#testSignatureHelpDescriptionDisabled` |
| `description_enabled` | `SignatureHelpHandlerTest#testSignatureHelpDescriptionEnabled` |
| `diamond` | `SignatureHelpHandlerTest#testSignatureHelp_diamond` |
| `end_of_doc` | `SignatureHelpHandlerTest#testSignatureHelp_endOfDoc` |
| `for_selected_completion_proposal` | `SignatureHelpHandlerTest#testSignatureHelpForSelectedCompletionProposal` |
| `generic_types2` | `SignatureHelpHandlerTest#testSignatureHelp_genericTypes2` |
| `generic_types3` | `SignatureHelpHandlerTest#testSignatureHelp_genericTypes3` |
| `invalid_ast` | `SignatureHelpHandlerTest#testSignatureHelp_invalidAST` |
| `javadoc` | `SignatureHelpHandlerTest#testSignatureHelp_javadoc` |
| `javadoc_original` | `SignatureHelpHandlerTest#testSignatureHelp_javadocOriginal` |
| `lambda` | `SignatureHelpHandlerTest#testSignatureHelp_lambda` |
| `lambda2_1` | `SignatureHelpHandlerTest#testSignatureHelp_lambda2` |
| `lambda2_2` | `SignatureHelpHandlerTest#testSignatureHelp_lambda2` |
| `method_1` | `SignatureHelpHandlerTest#testSignatureHelpMethod` |
| `method_2` | `SignatureHelpHandlerTest#testSignatureHelpMethod` |
| `method_3` | `SignatureHelpHandlerTest#testSignatureHelpMethod` |
| `overloads` | `SignatureHelpHandlerTest#testSignatureHelp_overloads` |
| `parameter_documentation` | `SignatureHelpHandlerTest#testSignatureHelp_parameterDocumentation` |
| `skip` | `SignatureHelpHandlerTest#testSignatureHelp_skip` |
| `super_method` | `SignatureHelpHandlerTest#testSignatureHelp_superMethod` |
| `text_block` | `SignatureHelpHandlerTest#testSignatureHelp_textBlock` |
| `varargs2` | `SignatureHelpHandlerTest#testSignatureHelp_varargs2` |
| `varargs_1` | `SignatureHelpHandlerTest#testSignatureHelp_varargs` |
| `varargs_2` | `SignatureHelpHandlerTest#testSignatureHelp_varargs` |

### definition

| case | jdtls test |
| --- | --- |
| `kotlin` | `NavigateToDefinitionHandlerTest#testKotlin` |

### declaration

Answer: the places, as **definition** writes them.

| case | jdtls test |
| --- | --- |
| `custom_package` | `NavigateToDeclarationHandlerTest#testCustomPackage` |
| `get_field_declaration` | `NavigateToDeclarationHandlerTest#testGetFieldDeclaration` |
| `get_method_declaration` | `NavigateToDeclarationHandlerTest#testGetMethodDeclaration` |
| `get_method_declaration_same_file` | `NavigateToDeclarationHandlerTest#testGetMethodDeclarationSameFile` |

### references

| case | jdtls test |
| --- | --- |
| `enum_in_class_file` | `ReferencesHandlerTest#testEnumInClassFile` |
| `include_accessors` | `ReferencesHandlerTest#testIncludeAccessors` |
| `project_only_search_scope` | `ReferencesHandlerTest#testProjectOnlySearchScope` |

### implementation

| case | jdtls test |
| --- | --- |
| `from_binary_type_with_class_content_support` | `ImplementationsHandlerTest#testImplementationFromBinaryTypeWithClassContentSupport` |

### rename

| case | jdtls test |
| --- | --- |
| `type_with_errors` | `RenameHandlerTest#testRenameTypeWithErrors` |

### document_symbol

| case | jdtls test |
| --- | --- |
| `deprecated` | `DocumentSymbolHandlerTest#testDeprecated` |
| `deprecated_property` | `DocumentSymbolHandlerTest#testDeprecated_property` |

### workspace_symbol

| case | jdtls test |
| --- | --- |
| `project_only_search_scope` | `WorkspaceSymbolHandlerTest#testProjectOnlySearchScope` |
| `search_source_method_declarations` | `WorkspaceSymbolHandlerTest#testSearchSourceMethodDeclarations` |
| `search_source_only` | `WorkspaceSymbolHandlerTest#testSearchSourceOnly` |
| `workspace_search_on_file_in_workspace` | `WorkspaceSymbolHandlerTest#testWorkspaceSearchOnFileInWorkspace` |

### diagnostics

| case | jdtls test |
| --- | --- |
| `annotation` | `WorkspaceDiagnosticsHandlerTest#testAnnotation` |
| `bad_location_exception` | `WorkspaceDiagnosticsHandlerTest#testBadLocationException` |
| `basic_buffer_life_cycle` | `DocumentLifeCycleHandlerTest#testBasicBufferLifeCycle` |
| `basic_buffer_life_cycle_without_save` | `DocumentLifeCycleHandlerTest#testBasicBufferLifeCycleWithoutSave` |
| `close_missing_resource` | `DocumentLifeCycleHandlerTest#testCloseMissingResource` |
| `delete_package` | `WorkspaceDiagnosticsHandlerTest#testDeletePackage` |
| `deprecated` | `DiagnosticHandlerTest#testDeprecated` |
| `diagnostic_filtering` | `WorkspaceDiagnosticsHandlerTest#testDiagnosticFiltering` |
| `did_open_not_on_classpath` | `DocumentLifeCycleHandlerTest#testDidOpenNotOnClasspath` |
| `did_open_standalone_file` | `DocumentLifeCycleHandlerTest#testDidOpenStandaloneFile` |
| `did_open_standalone_file_with_non_syntax_errors` | `DocumentLifeCycleHandlerTest#testDidOpenStandaloneFileWithNonSyntaxErrors` |
| `did_open_standalone_file_with_syntax_error` | `DocumentLifeCycleHandlerTest#testDidOpenStandaloneFileWithSyntaxError` |
| `document_monitor` | `DocumentLifeCycleHandlerTest#testDocumentMonitor` |
| `document_monitor_closed_document` | `DocumentLifeCycleHandlerTest#testDocumentMonitorClosedDocument` |
| `file_rename_for_type_declaration` | `DocumentLifeCycleHandlerTest#testFileRenameForTypeDeclaration` |
| `fix_in_dependency_scenario` | `DocumentLifeCycleHandlerTest#testFixInDependencyScenario` |
| `incremental_change_document` | `DocumentLifeCycleHandlerTest#testIncrementalChangeDocument` |
| `multiple_line_range` | `DiagnosticHandlerTest#testMultipleLineRange` |
| `non_jdt_error` | `DocumentLifeCycleHandlerTest#testNonJdtError` |
| `not_used` | `DiagnosticHandlerTest#testNotUsed` |
| `reconcile` | `DocumentLifeCycleHandlerTest#testReconcile` |
| `remove_dead_code_after_if` | `DocumentLifeCycleHandlerTest#testRemoveDeadCodeAfterIf` |
| `static_reference` | `DiagnosticHandlerTest#testStaticReference` |
| `task` | `DiagnosticHandlerTest#testTask` |
| `task_markers` | `WorkspaceDiagnosticsHandlerTest#testTaskMarkers` |
| `unimplemented_methods` | `DocumentLifeCycleHandlerTest#testUnimplementedMethods` |
| `unnecessary` | `DiagnosticHandlerTest#testUnnecessary` |
| `unused_lambda_parameter_warning_disabled_by_default` | `DiagnosticHandlerTest#testUnusedLambdaParameterWarningDisabledByDefault` |
| `without_working_copy` | `WorkspaceDiagnosticsHandlerTest#testWithoutWorkingCopy` |
| `working_copies` | `DocumentLifeCycleHandlerTest#testWorkingCopies` |
| `working_copy` | `WorkspaceDiagnosticsHandlerTest#testWorkingCopy` |
| `working_copy2` | `WorkspaceDiagnosticsHandlerTest#testWorkingCopy2` |

### code_action

Answer: the title of each action, sorted; asked for the selection `$0`…`$1` (or the empty range at `$0`) with no diagnostics in the context, so quick fixes that need one show up only where the server finds the problem itself.

| case | jdtls test |
| --- | --- |
| `advanced_generate_accessors_enabled` | `GenerateAccessorsActionTest#testAdvancedGenerateAccessorsEnabled` |
| `all_kinds_of_actions` | `CodeActionHandlerTest#testCodeAction_allKindsOfActions` |
| `ambiguous_static_imports` | `AdvancedOrganizeImportsHandlerTest#testAmbiguousStaticImports` |
| `assign_all_params_to_fields` | `CodeActionResolveHandlerTest#testAssignAllParamsToFields` |
| `choose_import` | `AdvancedOrganizeImportsHandlerTest#testChooseImport` |
| `custom_file_formatting_options` | `CodeActionHandlerTest#testCodeAction_customFileFormattingOptions` |
| `duplicate_static_imports` | `AdvancedOrganizeImportsHandlerTest#testDuplicateStaticImports` |
| `error_from_other_sources` | `CodeActionHandlerTest#testCodeAction_errorFromOtherSources` |
| `exception` | `CodeActionHandlerTest#testCodeAction_exception` |
| `filter_types` | `CodeActionHandlerTest#test_filterTypes` |
| `generate_accessors_disabled_empty_fields` | `GenerateAccessorsActionTest#testGenerateAccessorsDisabled_emptyFields` |
| `generate_accessors_disabled_interface` | `GenerateAccessorsActionTest#testGenerateAccessorsDisabled_interface` |
| `generate_accessors_enabled` | `GenerateAccessorsActionTest#testGenerateAccessorsEnabled` |
| `generate_accessors_for_record_enabled` | `GenerateAccessorsActionTest#testGenerateAccessorsForRecordEnabled` |
| `generate_accessors_quick_assist_at_line` | `GenerateAccessorsActionTest#testGenerateAccessorsQuickAssistAtLine` |
| `generate_accessors_quick_assist_at_line1` | `GenerateAccessorsActionTest#testGenerateAccessorsQuickAssistAtLine1` |
| `generate_accessors_quick_assist_for_field_declaration` | `GenerateAccessorsActionTest#testGenerateAccessorsQuickAssistForFieldDeclaration` |
| `generate_accessors_quick_assist_for_final_field` | `GenerateAccessorsActionTest#testGenerateAccessorsQuickAssistForFinalField` |
| `generate_accessors_quick_assist_for_multiple_field_declaration` | `GenerateAccessorsActionTest#testGenerateAccessorsQuickAssistForMultipleFieldDeclaration` |
| `generate_accessors_quick_assist_for_type_declaration` | `GenerateAccessorsActionTest#testGenerateAccessorsQuickAssistForTypeDeclaration` |
| `generate_add_modifer` | `AddFieldModifierActionTest#testGenerateAddModifer` |
| `generate_change_modifier` | `AddFieldModifierActionTest#testGenerateChangeModifier` |
| `generate_constructors_disabled_anonymous` | `GenerateConstructorsActionTest#testGenerateConstructorsDisabled_anonymous` |
| `generate_constructors_disabled_interface` | `GenerateConstructorsActionTest#testGenerateConstructorsDisabled_interface` |
| `generate_constructors_empty_fields` | `GenerateConstructorsActionTest#testGenerateConstructors_emptyFields` |
| `generate_constructors_enabled` | `GenerateConstructorsActionTest#testGenerateConstructorsEnabled` |
| `generate_constructors_quick_assist_1` | `GenerateConstructorsActionTest#testGenerateConstructorsQuickAssist` |
| `generate_constructors_quick_assist_2` | `GenerateConstructorsActionTest#testGenerateConstructorsQuickAssist` |
| `generate_constructors_quick_assist_with_all_static_fields_1` | `GenerateConstructorsActionTest#testGenerateConstructorsQuickAssistWithAllStaticFields` |
| `generate_constructors_quick_assist_with_all_static_fields_2` | `GenerateConstructorsActionTest#testGenerateConstructorsQuickAssistWithAllStaticFields` |
| `generate_constructors_with_super_delegation` | `GenerateConstructorsActionTest#testGenerateConstructorsWithSuperDelegation` |
| `generate_delegate_methods_disabled` | `GenerateDelegateMethodsActionTest#testGenerateDelegateMethodsDisabled` |
| `generate_delegate_methods_disabled_interface` | `GenerateDelegateMethodsActionTest#testGenerateDelegateMethodsDisabled_interface` |
| `generate_delegate_methods_enabled` | `GenerateDelegateMethodsActionTest#testGenerateDelegateMethodsEnabled` |
| `generate_to_string_disabled_enum` | `GenerateToStringActionTest#testGenerateToStringDisabled_enum` |
| `generate_to_string_disabled_interface` | `GenerateToStringActionTest#testGenerateToStringDisabled_interface` |
| `generate_to_string_enabled` | `GenerateToStringActionTest#testGenerateToStringEnabled` |
| `generate_to_string_enabled_empty_fields` | `GenerateToStringActionTest#testGenerateToStringEnabled_emptyFields` |
| `generate_to_string_quick_assist_1` | `GenerateToStringActionTest#testGenerateToStringQuickAssist` |
| `generate_to_string_quick_assist_2` | `GenerateToStringActionTest#testGenerateToStringQuickAssist` |
| `generate_to_string_quick_assist_empty_fields_1` | `GenerateToStringActionTest#testGenerateToStringQuickAssist_emptyFields` |
| `generate_to_string_quick_assist_empty_fields_2` | `GenerateToStringActionTest#testGenerateToStringQuickAssist_emptyFields` |
| `gh3905` | `CodeActionResolveHandlerTest#testGH3905` |
| `hash_code_equals_disabled_empty_fields` | `HashCodeEqualsActionTest#testHashCodeEqualsDisabled_emptyFields` |
| `hash_code_equals_disabled_enum` | `HashCodeEqualsActionTest#testHashCodeEqualsDisabled_enum` |
| `hash_code_equals_disabled_interface` | `HashCodeEqualsActionTest#testHashCodeEqualsDisabled_interface` |
| `hash_code_equals_enabled` | `HashCodeEqualsActionTest#testHashCodeEqualsEnabled` |
| `hash_code_equals_quick_assist_1` | `HashCodeEqualsActionTest#testHashCodeEqualsQuickAssist` |
| `hash_code_equals_quick_assist_2` | `HashCodeEqualsActionTest#testHashCodeEqualsQuickAssist` |
| `ignore_compiler_issue` | `CodeActionHandlerTest#testCodeAction_ignoreCompilerIssue` |
| `ignoring_other_diagnostic_without_code` | `CodeActionHandlerTest#testCodeAction_ignoringOtherDiagnosticWithoutCode` |
| `insert_final_modifier_for_field_declaration_quick_assist_1` | `GenerateFinalModifiersQuickAssistTest#testInsertFinalModifierForFieldDeclarationQuickAssist` |
| `insert_final_modifier_for_field_declaration_quick_assist_2` | `GenerateFinalModifiersQuickAssistTest#testInsertFinalModifierForFieldDeclarationQuickAssist` |
| `insert_final_modifier_for_field_declaration_quick_assist_3` | `GenerateFinalModifiersQuickAssistTest#testInsertFinalModifierForFieldDeclarationQuickAssist` |
| `insert_final_modifier_for_field_declarations_quick_assist` | `GenerateFinalModifiersQuickAssistTest#testInsertFinalModifierForFieldDeclarationsQuickAssist` |
| `insert_final_modifier_for_local_variable_quick_assist_1` | `GenerateFinalModifiersQuickAssistTest#testInsertFinalModifierForLocalVariableQuickAssist` |
| `insert_final_modifier_for_local_variable_quick_assist_2` | `GenerateFinalModifiersQuickAssistTest#testInsertFinalModifierForLocalVariableQuickAssist` |
| `insert_final_modifier_for_local_variable_quick_assist_3` | `GenerateFinalModifiersQuickAssistTest#testInsertFinalModifierForLocalVariableQuickAssist` |
| `insert_final_modifier_for_local_variable_selection_quick_assist_1` | `GenerateFinalModifiersQuickAssistTest#testInsertFinalModifierForLocalVariableSelectionQuickAssist` |
| `insert_final_modifier_for_local_variable_selection_quick_assist_2` | `GenerateFinalModifiersQuickAssistTest#testInsertFinalModifierForLocalVariableSelectionQuickAssist` |
| `insert_final_modifier_for_local_variable_selection_quick_assist_3` | `GenerateFinalModifiersQuickAssistTest#testInsertFinalModifierForLocalVariableSelectionQuickAssist` |
| `insert_final_modifier_for_parameter_quick_assist_1` | `GenerateFinalModifiersQuickAssistTest#testInsertFinalModifierForParameterQuickAssist` |
| `insert_final_modifier_for_parameter_quick_assist_2` | `GenerateFinalModifiersQuickAssistTest#testInsertFinalModifierForParameterQuickAssist` |
| `insert_final_modifier_where_possible` | `GenerateFinalModifiersActionTest#testInsertFinalModifierWherePossible` |
| `literal_remove_unterminated_string` | `CodeActionHandlerTest#testCodeActionLiteral_removeUnterminatedString` |
| `literal_remove_unused_import` | `CodeActionHandlerTest#testCodeActionLiteral_removeUnusedImport` |
| `no_generate_to_string_quick_assist` | `GenerateToStringActionTest#testNoGenerateToStringQuickAssist` |
| `no_hash_code_equals_quick_assist` | `HashCodeEqualsActionTest#testNoHashCodeEqualsQuickAssist` |
| `no_unnecessary_code_actions` | `CodeActionHandlerTest#test_noUnnecessaryCodeActions` |
| `npe_in_new_cu_proposal` | `CodeActionHandlerTest#testCodeAction_NPEInNewCUProposal` |
| `npe_on_invalid_method_call` | `CodeActionHandlerTest#testCodeAction_NPEOnInvalidMethodCall` |
| `organize_imports_choice_and_cancellation` | `AdvancedOrganizeImportsHandlerTest#testOrganizeImportsChoiceAndCancellation` |
| `organize_imports_quick_fix_1` | `CodeActionHandlerTest#testCodeAction_organizeImportsQuickFix` |
| `organize_imports_quick_fix_2` | `CodeActionHandlerTest#testCodeAction_organizeImportsQuickFix` |
| `organize_imports_source_action_only` | `CodeActionHandlerTest#testCodeAction_organizeImportsSourceActionOnly` |
| `override_methods_for_annotations` | `OverrideMethodsActionTest#testOverrideMethodsForAnnotations` |
| `override_methods_quick_assist_1` | `OverrideMethodsActionTest#testOverrideMethodsQuickAssist` |
| `override_methods_quick_assist_2` | `OverrideMethodsActionTest#testOverrideMethodsQuickAssist` |
| `quick_assist_for_field_declaration_order` | `CodeActionHandlerTest#testQuickAssistForFieldDeclarationOrder` |
| `quick_assist_for_import_declaration_order` | `CodeActionHandlerTest#testQuickAssistForImportDeclarationOrder` |
| `quick_assist_for_type_declaration_order` | `CodeActionHandlerTest#testQuickAssistForTypeDeclarationOrder` |
| `quickfix_actions_only` | `CodeActionHandlerTest#testCodeAction_quickfixActionsOnly` |
| `refactor_actions_only` | `CodeActionHandlerTest#testCodeAction_refactorActionsOnly` |
| `refresh_diagnostics_command_in_new_cu_proposal` | `CodeActionHandlerTest#testCodeAction_refreshDiagnosticsCommandInNewCUProposal` |
| `remove_static_imports` | `AdvancedOrganizeImportsHandlerTest#testRemoveStaticImports` |
| `remove_unterminated_string` | `CodeActionHandlerTest#testCodeAction_removeUnterminatedString` |
| `remove_unused_import` | `CodeActionHandlerTest#testCodeAction_removeUnusedImport` |
| `resolve_code_action_annotation_quick_fixes` | `CodeActionResolveHandlerTest#testResolveCodeAction_AnnotationQuickFixes` |
| `resolve_code_action_linked_proposals` | `CodeActionResolveHandlerTest#testResolveCodeAction_LinkedProposals` |
| `resolve_code_action_quick_assists` | `CodeActionResolveHandlerTest#testResolveCodeAction_QuickAssists` |
| `resolve_code_action_quick_fixes` | `CodeActionResolveHandlerTest#testResolveCodeAction_QuickFixes` |
| `resolve_code_action_refactors` | `CodeActionResolveHandlerTest#testResolveCodeAction_Refactors` |
| `resolve_code_action_source_actions` | `CodeActionResolveHandlerTest#testResolveCodeAction_SourceActions` |
| `sort_member_action_exists` | `SortMembersActionTest#testSortMemberActionExists` |
| `sort_member_action_exists_with_volatile_changes` | `SortMembersActionTest#testSortMemberActionExistsWithVolatileChanges` |
| `sort_member_action_not_exists` | `SortMembersActionTest#testSortMemberActionNotExists` |
| `sort_member_quick_assist_exists_for_selection` | `SortMembersActionTest#testSortMemberQuickAssistExistsForSelection` |
| `sort_member_quick_assist_exists_for_type_declaration` | `SortMembersActionTest#testSortMemberQuickAssistExistsForTypeDeclaration` |
| `source_action_and_proposal_add_module_requirement` | `AdvancedOrganizeImportsHandlerTest#testSourceActionAndProposalAddModuleRequirement` |
| `source_actions_only` | `CodeActionHandlerTest#testCodeAction_sourceActionsOnly` |
| `static_imports` | `AdvancedOrganizeImportsHandlerTest#testStaticImports` |
| `superfluous_semicolon` | `CodeActionHandlerTest#testCodeAction_superfluousSemicolon` |
| `unimplemented_method_reference` | `CodeActionHandlerTest#testCodeAction_unimplementedMethodReference` |

### formatting

Answer: the cursor's file after the edits, line by line (tab indentation, tab size 4); a case with `$0`…`$1` asks `rangeFormatting` for that selection.

| case | jdtls test |
| --- | --- |
| `disable_formatting_on_type` | `FormatterHandlerTest#testDisableFormattingOnType` |
| `document_formatting` | `FormatterHandlerTest#testDocumentFormatting` |
| `document_formatting_with_custom_option` | `FormatterHandlerTest#testDocumentFormattingWithCustomOption` |
| `document_formatting_with_tabs` | `FormatterHandlerTest#testDocumentFormattingWithTabs` |
| `google_formatter` | `FormatterHandlerTest#testGoogleFormatter` |
| `google_formatter_file_path` | `FormatterHandlerTest#testGoogleFormatterFilePath` |
| `indent_switchstatements_default` | `FormatterHandlerTest#testFormatting_indent_switchstatementsDefault` |
| `indent_switchstatements_false` | `FormatterHandlerTest#testFormatting_indent_switchstatementsFalse` |
| `java_format_enable` | `FormatterHandlerTest#testJavaFormatEnable` |
| `on_off_tags` | `FormatterHandlerTest#testFormatting_onOffTags` |
| `on_type_close_block` | `FormatterHandlerTest#testFormattingOnTypeCloseBlock` |
| `on_type_new_line` | `FormatterHandlerTest#testFormattingOnTypeNewLine` |
| `on_type_return_after_closed_block` | `FormatterHandlerTest#testFormattingOnTypeReturnAfterClosedBlock` |
| `on_type_return_after_empty_block` | `FormatterHandlerTest#testFormattingOnTypeReturnAfterEmptyBlock` |
| `on_type_return_after_empty_line` | `FormatterHandlerTest#testFormattingOnTypeReturnAfterEmptyLine` |
| `on_type_return_after_opening_block` | `FormatterHandlerTest#testFormattingOnTypeReturnAfterOpeningBlock` |
| `on_type_return_midline` | `FormatterHandlerTest#testFormattingOnTypeReturnMidline` |
| `on_type_semi_column` | `FormatterHandlerTest#testFormattingOnTypeSemiColumn` |
| `range_formatting` | `FormatterHandlerTest#testRangeFormatting` |
| `update_formatter_version` | `FormatterHandlerTest#testUpdateFormatterVersion` |

### inlay_hint

Answer: `line:column  label` of each hint in the whole file.

| case | jdtls test |
| --- | --- |
| `all_mode` | `InlayHintHandlerTest#testAllMode` |
| `boolean_literal` | `InlayHintHandlerTest#testBooleanLiteral` |
| `character_literal` | `InlayHintHandlerTest#testCharacterLiteral` |
| `complex_expressions` | `InlayHintHandlerTest#testComplexExpressions` |
| `constructor_invocation` | `InlayHintHandlerTest#testConstructorInvocation` |
| `disable_same_named_numbered_parameter_filter` | `InlayHintHandlerTest#testDisableSameNamedNumberedParameterFilter` |
| `disabled_lambda_parameter_type_hints` | `InlayHintHandlerTest#testDisabledLambdaParameterTypeHints` |
| `enum_constant` | `InlayHintHandlerTest#testEnumConstant` |
| `format_specifier_disabled` | `InlayHintHandlerTest#testFormatSpecifierDisabled` |
| `format_specifier_escaped_percent` | `InlayHintHandlerTest#testFormatSpecifierEscapedPercent` |
| `format_specifier_explicit_arg_index` | `InlayHintHandlerTest#testFormatSpecifierExplicitArgIndex` |
| `format_specifier_formatted` | `InlayHintHandlerTest#testFormatSpecifierFormatted` |
| `format_specifier_formatter_format` | `InlayHintHandlerTest#testFormatSpecifierFormatterFormat` |
| `format_specifier_multiple_args` | `InlayHintHandlerTest#testFormatSpecifierMultipleArgs` |
| `format_specifier_newline_does_not_consume_arg` | `InlayHintHandlerTest#testFormatSpecifierNewlineDoesNotConsumeArg` |
| `format_specifier_non_string_format` | `InlayHintHandlerTest#testFormatSpecifierNonStringFormat` |
| `format_specifier_print_stream_format` | `InlayHintHandlerTest#testFormatSpecifierPrintStreamFormat` |
| `format_specifier_printf_print_stream` | `InlayHintHandlerTest#testFormatSpecifierPrintfPrintStream` |
| `format_specifier_single_arg` | `InlayHintHandlerTest#testFormatSpecifierSingleArg` |
| `format_specifier_static_format_with_locale` | `InlayHintHandlerTest#testFormatSpecifierStaticFormatWithLocale` |
| `format_specifier_text_block` | `InlayHintHandlerTest#testFormatSpecifierTextBlock` |
| `lambda_explicit_type_no_hints` | `InlayHintHandlerTest#testLambdaExplicitTypeNoHints` |
| `lambda_parameter_type_hints` | `InlayHintHandlerTest#testLambdaParameterTypeHints` |
| `new_expression` | `InlayHintHandlerTest#testNewExpression` |
| `no_inlay_hint_for_non_literal` | `InlayHintHandlerTest#testNoInlayHintForNonLiteral` |
| `no_inlay_hints_for_cast_expression` | `InlayHintHandlerTest#testNoInlayHintsForCastExpression` |
| `no_inlay_hints_when_names_are_equal` | `InlayHintHandlerTest#testNoInlayHintsWhenNamesAreEqual` |
| `none_mode` | `InlayHintHandlerTest#testNoneMode` |
| `null_literal` | `InlayHintHandlerTest#testNullLiteral` |
| `number_literal` | `InlayHintHandlerTest#testNumberLiteral` |
| `out_of_range` | `InlayHintHandlerTest#testOutOfRange` |
| `record` | `InlayHintHandlerTest#testRecord` |
| `same_prefix_parameter_filter` | `InlayHintHandlerTest#testSamePrefixParameterFilter` |
| `same_prefix_parameter_filter_different_prefixes` | `InlayHintHandlerTest#testSamePrefixParameterFilterDifferentPrefixes` |
| `same_prefix_parameter_filter_longer_prefix` | `InlayHintHandlerTest#testSamePrefixParameterFilterLongerPrefix` |
| `same_prefix_parameter_filter_mixed` | `InlayHintHandlerTest#testSamePrefixParameterFilterMixed` |
| `same_prefix_parameter_filter_non_consecutive` | `InlayHintHandlerTest#testSamePrefixParameterFilterNonConsecutive` |
| `same_prefix_parameter_filter_not_starting_with_one` | `InlayHintHandlerTest#testSamePrefixParameterFilterNotStartingWithOne` |
| `same_prefix_parameter_filter_single_param` | `InlayHintHandlerTest#testSamePrefixParameterFilterSingleParam` |
| `string_literal` | `InlayHintHandlerTest#testStringLiteral` |
| `super_constructor_invocation` | `InlayHintHandlerTest#testSuperConstructorInvocation` |
| `super_method_invocation` | `InlayHintHandlerTest#testSuperMethodInvocation` |
| `type_literal` | `InlayHintHandlerTest#testTypeLiteral` |
| `varargs` | `InlayHintHandlerTest#testVarargs` |
| `varargs2` | `InlayHintHandlerTest#testVarargs2` |
| `variable_type_hints` | `InlayHintHandlerTest#testVariableTypeHints` |

### code_lens

Answer: `/path:line:column  source line` of each lens, with `[title]` when the server already resolved its command.

| case | jdtls test |
| --- | --- |
| `disable_code_lens_symbols` | `CodeLensHandlerTest#testDisableCodeLensSymbols` |
| `disable_implementations_code_lens_symbols` | `CodeLensHandlerTest#testDisableImplementationsCodeLensSymbols` |
| `enable_implementations_code_lens_symbols` | `CodeLensHandlerTest#testEnableImplementationsCodeLensSymbols` |
| `enable_implementations_code_lens_symbols_for_base_types` | `CodeLensHandlerTest#testEnableImplementationsCodeLensSymbolsForBaseTypes` |
| `get_code_lens_symbols` | `CodeLensHandlerTest#testGetCodeLensSymbols` |
| `get_code_lens_symbols_include_fields` | `CodeLensHandlerTest#testGetCodeLensSymbolsIncludeFields` |
| `ignore_lombok_code_lens_symbols` | `CodeLensHandlerTest#testIgnoreLombokCodeLensSymbols` |
| `no_reference_code_lens_for_unnamed_classes` | `CodeLensHandlerTest#testNoReferenceCodeLensForUnnamedClasses` |
| `resolve_code_lense` | `CodeLensHandlerTest#testResolveCodeLense` |

### call_hierarchy

Answer: `item`, `incoming` and `outgoing` lines: `name  /path:line:column  source line`, for the first item prepared at the cursor.

jdtls names a member as `of(T) <T> : Opt<T>`, `Base()`, `protectedField`,
`Opt<T>`, and shows a caller at its first call of the item (the `new` of a
creation, the `super` of a chained constructor), not at its own name; a
callee is shown at its name. The expectations were recorded with jdtls
1.63.0 (`compat.rs`).

| case | jdtls test |
| --- | --- |
| `incoming_calls_maven` | `CallHierarchyHandlerTest#incomingCalls_maven` |
| `incoming_calls_on_interface_method` | `CallHierarchyHandlerTest#incomingCalls_OnInterfaceMethod` |
| `incoming_calls_on_interface_method_return_type` | `CallHierarchyHandlerTest#incomingCalls_OnInterfaceMethodReturnType` |
| `incoming_calls_project_only` | `CallHierarchyHandlerTest#incomingCalls_projectOnly` |
| `incoming_calls_search_scope_changed` | `CallHierarchyHandlerTest#incomingCalls_searchScopeChanged` |
| `incoming_calls_src` | `CallHierarchyHandlerTest#incomingCalls_src` |
| `outgoing_calls_src` | `CallHierarchyHandlerTest#outgoingCalls_src` |
| `outgoing_jar` | `CallHierarchyHandlerTest#outgoing_jar` |
| `prepare_call_hierarchy` | `CallHierarchyHandlerTest#prepareCallHierarchy` |
| `prepare_call_hierarchy_no_item_at_location` | `CallHierarchyHandlerTest#prepareCallHierarchy_noItemAtLocation` |
| `prepare_call_hierarchy_src_enclosing` | `CallHierarchyHandlerTest#prepareCallHierarchy_src_enclosing` |
| `selection_range` | `CallHierarchyHandlerTest#testSelectionRange` |

### type_hierarchy

Answer: `item`, `supertype` and `subtype` lines, as **call_hierarchy** writes them.

| case | jdtls test |
| --- | --- |
| `method_hierarchy` | `TypeHierarchyHandlerTest#testMethodHierarchy` |
| `multiple_projects` | `TypeHierarchyHandlerTest#testMultipleProjects` |
| `sub_type_hierarchy` | `TypeHierarchyHandlerTest#testSubTypeHierarchy` |
| `super_type_hierarchy` | `TypeHierarchyHandlerTest#testSuperTypeHierarchy` |

### semantic_tokens

Answer: `line:column  text  type [modifiers]` of each token, decoded with the legend from `initialize`.

| case | jdtls test |
| --- | --- |
| `annotations` | `SemanticTokensHandlerTest#testSemanticTokens_Annotations` |
| `class_literals` | `SemanticTokensHandlerTest#testSemanticTokens_ClassLiterals` |
| `constructors` | `SemanticTokensHandlerTest#testSemanticTokens_Constructors` |
| `fields` | `SemanticTokensHandlerTest#testSemanticTokens_Fields` |
| `generic_class_with_bounded_type_parameter_and_supertype` | `SemanticTokensHandlerTest#testSemanticTokens_GenericClassWithBoundedTypeParameterAndSupertype` |
| `javadoc` | `SemanticTokensHandlerTest#testSemanticTokens_Javadoc` |
| `methods` | `SemanticTokensHandlerTest#testSemanticTokens_Methods` |
| `packages` | `SemanticTokensHandlerTest#testSemanticTokens_Packages` |
| `types` | `SemanticTokensHandlerTest#testSemanticTokens_Types` |
| `variables` | `SemanticTokensHandlerTest#testSemanticTokens_Variables` |

### selection_range

Answer: the ranges around the cursor, innermost first, as `line:column..line:column`.

| case | jdtls test |
| --- | --- |
| `comments_1` | `SelectionRangeHandlerTest#testComments` |
| `comments_2` | `SelectionRangeHandlerTest#testComments` |
| `comments_3` | `SelectionRangeHandlerTest#testComments` |
| `javadoc` | `SelectionRangeHandlerTest#testJavadoc` |
| `param_list` | `SelectionRangeHandlerTest#testParamList` |
| `string_literal` | `SelectionRangeHandlerTest#testStringLiteral` |
| `switch` | `SelectionRangeHandlerTest#testSwitch` |

### Not ported

91 of the jdtls tests left to port could not become a case:

- Its sources are built by a parameterized helper or a project not checked out here; to be written by hand. `CodeActionResolveHandlerTest#testCreateMethod`, `CodeActionResolveHandlerTest#testCreateMethodNoBufferValidation`, `CodeLensHandlerTest#testResolveImplementationsAbstractMethodCodeLens`, `CodeLensHandlerTest#testResolveImplementationsCodeLens`, `CodeLensHandlerTest#testResolveImplementationsInterfaceMethodCodeLens`, `CompletionHandlerTest#testCompletion_InvalidJavadoc`, `CompletionHandlerTest#testCompletion_interfaceMethodOverride`, `CompletionHandlerTest#testCompletion_interfaceMethodOverrideJava4`, `CompletionHandlerTest#testCompletion_interfaceMethodOverrideJava5`, `CompletionHandlerTest#testCompletion_interfaceMethodOverrideNoSnippet`, `HoverHandlerTest#testInvalidJavadoc`, `SemanticTokensHandlerTest#testSemanticTokens_Modules`
- A unit test of a jdtls class, not a request. `AdvancedOrganizeImportsHandlerTest#test`, `CompletionHandlerTest#main`, `CompletionRankingAggregationTest#testAddData`, `CompletionRankingAggregationTest#testAddDecorator`, `CompletionRankingAggregationTest#testAddNullData`, `CompletionRankingAggregationTest#testAddScore`, `HoverHandlerTest#testHtmlToMarkdownConverter_alreadyRenderedHtmlWithEmbeddedAtWord`
- Asks about a file that does not exist or lies outside the workspace. `CodeLensHandlerTest#testGetCodeLenseBoundaries`, `CodeLensHandlerTest#testResolveCodeLenseBoundaries`, `DocumentSymbolHandlerTest#testDocumentSymbolsOnPlainFile`, `HoverHandlerTest#testHoverStandalone`, `HoverHandlerTest#testMissingUnit`, `ImplementationsHandlerTest#testEmpty`, `NavigateToDeclarationHandlerTest#testGetEmptyDeclaration`, `NavigateToDefinitionHandlerTest#testGetEmptyDefinition`, `NavigateToTypeDefinitionHandlerTest#testGetEmptyDefinition`, `ReferencesHandlerTest#testEmpty`
- Asks about a JDK or library type through its class file or attached sources: a fixture has no class files. `CodeLensHandlerTest#testGetCodeLensSymbolsForClass`, `DocumentSymbolHandlerTest#testDocumentSymbolHandler`, `DocumentSymbolHandlerTest#testDocumentSymbolHandler_hierarchical`, `DocumentSymbolHandlerTest#testPackage_class`, `DocumentSymbolHandlerTest#testSyntheticMember`, `DocumentSymbolHandlerTest#testSyntheticMember_hierarchical`, `DocumentSymbolHandlerTest#testSyntheticMember_hierarchical_noSourceAttached`, `FoldingRangeHandlerTest#testFoldingRanges`, `HoverHandlerTest#testHoverThrowable`, `HoverHandlerTest#testLinkWhenClassContentSupported`, `HoverHandlerTest#testNoLinkWhenClassContentUnsupported`, `ImplementationsHandlerTest#testImplementationFromBinaryTypeWithoutClassContentSupport`, `NavigateToDefinitionHandlerTest#testAttachedSource`, `NavigateToDefinitionHandlerTest#testDisassembledSource`, `NavigateToDefinitionHandlerTest#testJdkClasses`, `NavigateToDefinitionHandlerTest#testNoClassContentSupport`, `NavigateToDefinitionHandlerTest#testSourceVersion`, `NavigateToTypeDefinitionHandlerTest#testAttachedSource`, `NavigateToTypeDefinitionHandlerTest#testDisassembledSource`, `NavigateToTypeDefinitionHandlerTest#testNoClassContentSupport`, `ReferencesHandlerTest#testReferencesInJRE`, `SemanticTokensHandlerTest#testSemanticTokens_SourceAttachment`, `SignatureHelpHandlerTest#testSignatureHelpInClassFile`, `WorkspaceSymbolHandlerTest#testWorkspaceSearchNoClassContentSupport`, `WorkspaceSymbolHandlerTest#testWorkspaceSearchWithClassContentSupport`
- Its query is not an identifier in a source file (wildcards, packages, JDK types, result limits, settings), and a case asks for the word under the cursor. `WorkspaceSymbolHandlerTest#testDeprecated`, `WorkspaceSymbolHandlerTest#testDeprecated_property`, `WorkspaceSymbolHandlerTest#testEmptyNames`, `WorkspaceSymbolHandlerTest#testSearchAllTypesOfPackage`, `WorkspaceSymbolHandlerTest#testSearchPartialPackage`, `WorkspaceSymbolHandlerTest#testSearchQualifiedTypeWithWildcards`, `WorkspaceSymbolHandlerTest#testSearchReturnMaxResults`, `WorkspaceSymbolHandlerTest#testSearchWithoutDuplicate`, `WorkspaceSymbolHandlerTest#testWorkspaceSearch`
- Follows diagnostics through a sequence of edits, saves or project markers (Maven, natures, encodings): one fixture asks one question. `DocumentLifeCycleHandlerTest#testCreateCompilationUnit`, `DocumentLifeCycleHandlerTest#testDiagnosticsOnExternalFileWithInternalProject`, `DocumentLifeCycleHandlerTest#testDidOpenLazyLoadingInvisibleProject`, `DocumentLifeCycleHandlerTest#testNotExpectedPackage`, `DocumentLifeCycleHandlerTest#testNotExpectedPackage2`, `WorkspaceDiagnosticsHandlerTest#testEncoding`, `WorkspaceDiagnosticsHandlerTest#testMarkerListening`, `WorkspaceDiagnosticsHandlerTest#testMavenMarkers`, `WorkspaceDiagnosticsHandlerTest#testMissingDependencies`, `WorkspaceDiagnosticsHandlerTest#testMissingNatures`, `WorkspaceDiagnosticsHandlerTest#testProjectConfigurationIsNotUpToDate`, `WorkspaceDiagnosticsHandlerTest#testProjectLevelMarkers`, `WorkspaceDiagnosticsHandlerTest#testResetPomDiagnostics`, `WorkspaceDiagnosticsHandlerTest#testToDiagnosticsArray`
- Calls the jdtls formatter API (`stringFormatting`, profiles, notebooks, file paths), not an LSP request. `FormatterHandlerTest#testDefaultFormatterPreferences`, `FormatterHandlerTest#testFilePath`, `FormatterHandlerTest#testIndentSwitchStatements`, `FormatterHandlerTest#testJoinWrappedLines`, `FormatterHandlerTest#testNotebook`, `FormatterHandlerTest#testNotebook2`, `FormatterHandlerTest#testNotebookClass`, `FormatterHandlerTest#testNotebookMalformed`, `FormatterHandlerTest#testNotebookRange`, `FormatterHandlerTest#testProfileSettings`, `FormatterHandlerTest#testRelativeFilePath`, `FormatterHandlerTest#testStringFormatting`, `FormatterHandlerTest#testStringFormattingWithDefaultSettings`, `FormatterHandlerTest#testStringFormattingWithUpdating`

Not counted: tests of jdtls internals (initialization, build, project
import, settings, classpath), jdtls protocol extensions (`java/move`,
`java/paste`, the generate-code and change-signature prompts, save actions,
file renames) and tests jdtls has commented out.
