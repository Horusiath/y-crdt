use crate::{Any, Out, ReadTxn};
pub use jsonpath_rust::*;

/// An extension interface that enables resolving values out of [JsonPath] in context of a current
/// object.
trait JsonPathQuery {
    type Return: Default;

    /// Resolves [JsonPath] in the context of a current object.
    fn query(&self, json_path: &JsonPath) -> Vec<JsonPathValue<Self::Return>>;
}

impl<T> JsonPathQuery for Any {
    type Return = Any;

    fn query(&self, json_path: &JsonPath) -> Vec<JsonPathValue<Self::Return>> {
        todo!()
    }
}

impl<T> JsonPathQuery for T
where
    T: ReadTxn,
{
    type Return = Out;

    fn query(&self, json_path: &JsonPath) -> Vec<JsonPathValue<Self::Return>> {
        todo!()
    }
}

#[cfg(test)]
mod test {
    /*
       ORIGINAL TEST SUITE FROM: https://github.com/besok/jsonpath-rust/blob/main/src/jsonpath.rs
    */

    use crate::json_path::{jp_v, JsonPath, JsonPathQuery, JsonPathValue};
    use crate::{
        any, Any, Array, ArrayPrelim, ArrayRef, Doc, Map, MapPrelim, Out, Transact, WriteTxn,
    };
    use std::convert::{TryFrom, TryInto};
    use std::iter::FromIterator;

    fn test(doc: &Doc, path: &str, expected: Vec<JsonPathValue<Out>>) {
        let path = JsonPath::try_from(path).unwrap();
        assert_eq!(doc.transact().query(&path), expected)
    }

    fn test_doc() -> Doc {
        let doc = Doc::new();
        let mut tx = doc.transact_mut();

        let store = tx.get_or_insert_map("store");
        let book = store.insert(&mut tx, "book", ArrayPrelim::default());
        book.insert(
            &mut tx,
            0,
            MapPrelim::from_iter([
                ("category", Any::from("reference")),
                ("author", "Nigel Rees".into()),
                ("title", "Sayings of the Century".into()),
                ("price", (8.95).into()),
            ]),
        );
        book.insert(
            &mut tx,
            0,
            MapPrelim::from_iter([
                ("category", Any::from("fiction")),
                ("author", "Evelyn Waugh".into()),
                ("title", "Sword of Honour".into()),
                ("price", (12.99).into()),
            ]),
        );
        book.insert(
            &mut tx,
            0,
            MapPrelim::from_iter([
                ("category", Any::from("fiction")),
                ("author", "Herman Melville".into()),
                ("title", "Moby Dick".into()),
                ("isbn", "0-553-21311-3".into()),
                ("price", (8.99).into()),
            ]),
        );
        book.insert(
            &mut tx,
            0,
            MapPrelim::from_iter([
                ("category", Any::from("fiction")),
                ("author", "J. R. R. Tolkien".into()),
                ("title", "The Lord of the Rings".into()),
                ("isbn", "0-395-19395-8".into()),
                ("price", (22.99).into()),
            ]),
        );
        let bicycle = store.insert(
            &mut tx,
            "bicycle",
            MapPrelim::from_iter([("color", Any::from("red")), ("price", (19.95).into())]),
        );

        let array = tx.get_or_insert_array("array");
        array.insert_range(&mut tx, 0, [0, 1, 2, 3, 4, 5, 6, 7, 8, 9]);
        let orders = tx.get_or_insert_array("orders");
        orders.insert_range(
            &mut tx,
            0,
            [
                any!({
                    "ref":[1,2,3],
                    "id":1,
                    "filled": true
                }),
                any!({
                    "ref":[4,5,6],
                    "id":2,
                    "filled": false
                }),
                any!({
                    "ref":[7,8,9],
                    "id":3,
                    "filled": null
                }),
            ],
        );
        doc
    }

    #[test]
    fn descent_test() {
        let doc = test_doc();
        let v1 = any!("reference");
        let v2 = any!("fiction");
        test(
            &doc,
            "$..category",
            jp_v![
                 &v1;"$.['store'].['book'][0].['category']",
                 &v2;"$.['store'].['book'][1].['category']",
                 &v2;"$.['store'].['book'][2].['category']",
                 &v2;"$.['store'].['book'][3].['category']",],
        );
        let js1 = any!(19.95);
        let js2 = any!(8.95);
        let js3 = any!(12.99);
        let js4 = any!(8.99);
        let js5 = any!(22.99);
        test(
            &doc,
            "$.store..price",
            jp_v![
                &js1;"$.['store'].['bicycle'].['price']",
                &js2;"$.['store'].['book'][0].['price']",
                &js3;"$.['store'].['book'][1].['price']",
                &js4;"$.['store'].['book'][2].['price']",
                &js5;"$.['store'].['book'][3].['price']",
            ],
        );
        let js1 = any!("Nigel Rees");
        let js2 = any!("Evelyn Waugh");
        let js3 = any!("Herman Melville");
        let js4 = any!("J. R. R. Tolkien");
        test(
            &doc,
            "$..author",
            jp_v![
            &js1;"$.['store'].['book'][0].['author']",
            &js2;"$.['store'].['book'][1].['author']",
            &js3;"$.['store'].['book'][2].['author']",
            &js4;"$.['store'].['book'][3].['author']",],
        );
    }

    #[test]
    fn wildcard_test() {
        let doc = test_doc();
        let js1 = any!("reference");
        let js2 = any!("fiction");
        test(
            &doc,
            "$..book.[*].category",
            jp_v![
                &js1;"$.['store'].['book'][0].['category']",
                &js2;"$.['store'].['book'][1].['category']",
                &js2;"$.['store'].['book'][2].['category']",
                &js2;"$.['store'].['book'][3].['category']",],
        );
        let js1 = any!("Nigel Rees");
        let js2 = any!("Evelyn Waugh");
        let js3 = any!("Herman Melville");
        let js4 = any!("J. R. R. Tolkien");
        test(
            &doc,
            "$.store.book[*].author",
            jp_v![
                &js1;"$.['store'].['book'][0].['author']",
                &js2;"$.['store'].['book'][1].['author']",
                &js3;"$.['store'].['book'][2].['author']",
                &js4;"$.['store'].['book'][3].['author']",],
        );
    }

    #[test]
    fn descendent_wildcard_test() {
        let js1 = any!("Moby Dick");
        let js2 = any!("The Lord of the Rings");
        test(
            &test_doc(),
            "$..*.[?(@.isbn)].title",
            jp_v![
                &js1;"$.['store'].['book'][2].['title']",
                &js2;"$.['store'].['book'][3].['title']",
                &js1;"$.['store'].['book'][2].['title']",
                &js2;"$.['store'].['book'][3].['title']"],
        );
    }

    #[test]
    fn field_test() {
        let input = any!({"field":{"field":[{"active":1},{"passive":1}]}});
        let value = any!({"active":1});
        let path = JsonPath::try_from("$.field.field[?(@.active)]").unwrap();
        let actual = input.query(&path);
        assert_eq!(actual, jp_v![&value;"$.['field'].['field'][0]",]);
    }

    #[test]
    fn index_index_test() {
        let value = any!("0-553-21311-3");
        test(
            &test_doc(),
            "$..book[2].isbn",
            jp_v![&value;"$.['store'].['book'][2].['isbn']",],
        );
    }

    #[test]
    fn index_unit_index_test() {
        let doc = test_doc();
        let value = any!("0-553-21311-3");
        test(
            &doc,
            "$..book[2,4].isbn",
            jp_v![&value;"$.['store'].['book'][2].['isbn']",],
        );
        let value1 = any!("0-395-19395-8");
        test(
            &doc,
            "$..book[2,3].isbn",
            jp_v![&value;"$.['store'].['book'][2].['isbn']", &value1;"$.['store'].['book'][3].['isbn']",],
        );
    }

    #[test]
    fn index_unit_keys_test() {
        let js1 = any!("Moby Dick");
        let js2 = any!(8.99);
        let js3 = any!("The Lord of the Rings");
        let js4 = any!(22.99);
        test(
            &test_doc(),
            "$..book[2,3]['title','price']",
            jp_v![
                &js1;"$.['store'].['book'][2].['title']",
                &js2;"$.['store'].['book'][2].['price']",
                &js3;"$.['store'].['book'][3].['title']",
                &js4;"$.['store'].['book'][3].['price']",],
        );
    }

    #[test]
    fn index_slice_test() {
        let i0 = "$.['array'][0]";
        let i1 = "$.['array'][1]";
        let i2 = "$.['array'][2]";
        let i3 = "$.['array'][3]";
        let i4 = "$.['array'][4]";
        let i5 = "$.['array'][5]";
        let i6 = "$.['array'][6]";
        let i7 = "$.['array'][7]";
        let i8 = "$.['array'][8]";
        let i9 = "$.['array'][9]";

        let j0 = any!(0);
        let j1 = any!(1);
        let j2 = any!(2);
        let j3 = any!(3);
        let j4 = any!(4);
        let j5 = any!(5);
        let j6 = any!(6);
        let j7 = any!(7);
        let j8 = any!(8);
        let j9 = any!(9);
        let doc = test_doc();
        test(
            &doc,
            "$.array[:]",
            jp_v![
                &j0;&i0,
                &j1;&i1,
                &j2;&i2,
                &j3;&i3,
                &j4;&i4,
                &j5;&i5,
                &j6;&i6,
                &j7;&i7,
                &j8;&i8,
                &j9;&i9,],
        );
        test(&doc, "$.array[1:4:2]", jp_v![&j1;&i1, &j3;&i3,]);
        test(
            &doc,
            "$.array[::3]",
            jp_v![&j0;&i0, &j3;&i3, &j6;&i6, &j9;&i9,],
        );
        test(&doc, "$.array[-1:]", jp_v![&j9;&i9,]);
        test(&doc, "$.array[-2:-1]", jp_v![&j8;&i8,]);
    }

    #[test]
    fn index_filter_test() {
        let moby = any!("Moby Dick");
        let rings = any!("The Lord of the Rings");
        let doc = test_doc();
        test(
            &doc,
            "$..book[?(@.isbn)].title",
            jp_v![
                &moby;"$.['store'].['book'][2].['title']",
                &rings;"$.['store'].['book'][3].['title']",],
        );
        let sword = any!("Sword of Honour");
        test(
            &doc,
            "$..book[?(@.price != 8.95)].title",
            jp_v![
                &sword;"$.['store'].['book'][1].['title']",
                &moby;"$.['store'].['book'][2].['title']",
                &rings;"$.['store'].['book'][3].['title']",],
        );
        let sayings = any!("Sayings of the Century");
        test(
            &doc,
            "$..book[?(@.price == 8.95)].title",
            jp_v![&sayings;"$.['store'].['book'][0].['title']",],
        );
        let js895 = any!(8.95);
        test(
            &doc,
            "$..book[?(@.author ~= '.*Rees')].price",
            jp_v![&js895;"$.['store'].['book'][0].['price']",],
        );
        let js12 = any!(12.99);
        let js899 = any!(8.99);
        let js2299 = any!(22.99);
        test(
            &doc,
            "$..book[?(@.price >= 8.99)].price",
            jp_v![
                &js12;"$.['store'].['book'][1].['price']",
                &js899;"$.['store'].['book'][2].['price']",
                &js2299;"$.['store'].['book'][3].['price']",
            ],
        );
        test(
            &doc,
            "$..book[?(@.price > 8.99)].price",
            jp_v![
                &js12;"$.['store'].['book'][1].['price']",
                &js2299;"$.['store'].['book'][3].['price']",],
        );
        test(
            &doc,
            "$..book[?(@.price < 8.99)].price",
            jp_v![&js895;"$.['store'].['book'][0].['price']",],
        );
        test(
            &doc,
            "$..book[?(@.price <= 8.99)].price",
            jp_v![
                &js895;"$.['store'].['book'][0].['price']",
                &js899;"$.['store'].['book'][2].['price']",
            ],
        );
        test(
            &doc,
            "$..book[?(@.title in ['Moby Dick','Shmoby Dick','Big Dick','Dicks'])].price",
            jp_v![&js899;"$.['store'].['book'][2].['price']",],
        );
        test(
            &doc,
            "$..book[?(@.title nin ['Moby Dick','Shmoby Dick','Big Dick','Dicks'])].title",
            jp_v![
                &sayings;"$.['store'].['book'][0].['title']",
                &sword;"$.['store'].['book'][1].['title']",
                &rings;"$.['store'].['book'][3].['title']",],
        );
        test(
            &doc,
            "$..book[?(@.author size 10)].title",
            jp_v![&sayings;"$.['store'].['book'][0].['title']",],
        );
        let filled_true = any!(1);
        test(
            &doc,
            "$.orders[?(@.filled == true)].id",
            jp_v![&filled_true;"$.['orders'][0].['id']",],
        );
        let filled_null = any!(3);
        test(
            &doc,
            "$.orders[?(@.filled == null)].id",
            jp_v![&filled_null;"$.['orders'][2].['id']",],
        );
    }

    #[test]
    fn index_filter_sets_test() {
        let j1 = any!(1);
        let doc = test_doc();
        test(
            &doc,
            "$.orders[?(@.ref subsetOf [1,2,3,4])].id",
            jp_v![&j1;"$.['orders'][0].['id']",],
        );
        let j2 = any!(2);
        test(
            &doc,
            "$.orders[?(@.ref anyOf [1,4])].id",
            jp_v![&j1;"$.['orders'][0].['id']", &j2;"$.['orders'][1].['id']",],
        );
        let j3 = any!(3);
        test(
            &doc,
            "$.orders[?(@.ref noneOf [3,6])].id",
            jp_v![&j3;"$.['orders'][2].['id']",],
        );
    }

    #[test]
    fn query_test() {
        let doc = test_doc();
        let v = doc
            .transact()
            .query(&"$..book[?(@.author size 10)].title".try_into().unwrap());
        assert_eq!(v, any!(["Sayings of the Century"]));

        let path = doc
            .transact()
            .path("$..book[?(@.author size 10)].title")
            .expect("the path is correct");

        assert_eq!(path, &any!(["Sayings of the Century"]));
    }

    #[test]
    fn find_slice_test() {
        let doc = test_doc();
        let path: Box<JsonPath> = Box::from(
            JsonPath::try_from("$..book[?(@.author size 10)].title").expect("the path is correct"),
        );
        let v = path.find_slice(&json);
        let js = any!("Sayings of the Century");
        assert_eq!(v, jp_v![&js;"$.['store'].['book'][0].['title']",]);
    }

    #[test]
    fn find_in_array_test() {
        let json: Box<Value> = Box::new(any!([{"verb": "TEST"}, {"verb": "RUN"}]));
        let path: Box<JsonPath> =
            Box::from(JsonPath::try_from("$.[?(@.verb == 'TEST')]").expect("the path is correct"));
        let v = path.find_slice(&json);
        let js = any!({"verb":"TEST"});
        assert_eq!(v, jp_v![&js;"$[0]",]);
    }

    #[test]
    fn length_test() {
        let json: Box<Value> = Box::new(any!([{"verb": "TEST"},{"verb": "TEST"}, {"verb": "RUN"}]));
        let path: Box<JsonPath> = Box::from(
            JsonPath::try_from("$.[?(@.verb == 'TEST')].length()").expect("the path is correct"),
        );
        let v = path.find(&json);
        let js = any!([2]);
        assert_eq!(v, js);

        let json: Box<Value> = Box::new(any!([{"verb": "TEST"},{"verb": "TEST"}, {"verb": "RUN"}]));
        let path: Box<JsonPath> =
            Box::from(JsonPath::try_from("$.length()").expect("the path is correct"));
        assert_eq!(path.find(&json), any!([3]));

        // length of search following the wildcard returns correct result
        let json: Box<Value> =
            Box::new(any!([{"verb": "TEST"},{"verb": "TEST","x":3}, {"verb": "RUN"}]));
        let path: Box<JsonPath> = Box::from(
            JsonPath::try_from("$.[?(@.verb == 'TEST')].[*].length()")
                .expect("the path is correct"),
        );
        assert_eq!(path.find(&json), any!([3]));

        // length of object returns 0
        let json: Box<Value> = Box::new(any!({"verb": "TEST"}));
        let path: Box<JsonPath> =
            Box::from(JsonPath::try_from("$.length()").expect("the path is correct"));
        assert_eq!(path.find(&json), Value::Null);

        // length of integer returns null
        let json: Box<Value> = Box::new(any!(1));
        let path: Box<JsonPath> =
            Box::from(JsonPath::try_from("$.length()").expect("the path is correct"));
        assert_eq!(path.find(&json), Value::Null);

        // length of array returns correct result
        let json: Box<Value> = Box::new(any!([[1], [2], [3]]));
        let path: Box<JsonPath> =
            Box::from(JsonPath::try_from("$.length()").expect("the path is correct"));
        assert_eq!(path.find(&json), any!([3]));

        // path does not exist returns length null
        let json: Box<Value> = Box::new(any!([{"verb": "TEST"},{"verb": "TEST"}, {"verb": "RUN"}]));
        let path: Box<JsonPath> =
            Box::from(JsonPath::try_from("$.not.exist.length()").expect("the path is correct"));
        assert_eq!(path.find(&json), Value::Null);

        // seraching one value returns correct length
        let json: Box<Value> = Box::new(any!([{"verb": "TEST"},{"verb": "TEST"}, {"verb": "RUN"}]));
        let path: Box<JsonPath> = Box::from(
            JsonPath::try_from("$.[?(@.verb == 'RUN')].length()").expect("the path is correct"),
        );

        let v = path.find(&json);
        let js = any!([1]);
        assert_eq!(v, js);

        // searching correct path following unexisting key returns length 0
        let json: Box<Value> = Box::new(any!([{"verb": "TEST"},{"verb": "TEST"}, {"verb": "RUN"}]));
        let path: Box<JsonPath> = Box::from(
            JsonPath::try_from("$.[?(@.verb == 'RUN')].key123.length()")
                .expect("the path is correct"),
        );

        let v = path.find(&json);
        let js = any!(null);
        assert_eq!(v, js);

        // fetching first object returns length null
        let json: Box<Value> = Box::new(any!([{"verb": "TEST"},{"verb": "TEST"}, {"verb": "RUN"}]));
        let path: Box<JsonPath> =
            Box::from(JsonPath::try_from("$.[0].length()").expect("the path is correct"));

        let v = path.find(&json);
        let js = Value::Null;
        assert_eq!(v, js);

        // length on fetching the index after search gives length of the object (array)
        let json: Box<Value> = Box::new(any!([{"prop": [["a", "b", "c"], "d"]}]));
        let path: Box<JsonPath> = Box::from(
            JsonPath::try_from("$.[?(@.prop)].prop.[0].length()").expect("the path is correct"),
        );

        let v = path.find(&json);
        let js = any!([3]);
        assert_eq!(v, js);

        // length on fetching the index after search gives length of the object (string)
        let json: Box<Value> = Box::new(any!([{"prop": [["a", "b", "c"], "d"]}]));
        let path: Box<JsonPath> = Box::from(
            JsonPath::try_from("$.[?(@.prop)].prop.[1].length()").expect("the path is correct"),
        );

        let v = path.find(&json);
        let js = Value::Null;
        assert_eq!(v, js);
    }

    #[test]
    fn no_value_index_from_not_arr_filter_test() {
        let json: Box<Value> = Box::new(any!({
            "field":"field",
        }));

        let path: Box<JsonPath> =
            Box::from(JsonPath::try_from("$.field[1]").expect("the path is correct"));
        let v = path.find_slice(&json);
        assert_eq!(v, vec![NoValue]);

        let json: Box<Value> = Box::new(any!({
            "field":[0],
        }));

        let path: Box<JsonPath> =
            Box::from(JsonPath::try_from("$.field[1]").expect("the path is correct"));
        let v = path.find_slice(&json);
        assert_eq!(v, vec![NoValue]);
    }

    #[test]
    fn no_value_filter_from_not_arr_filter_test() {
        let json: Box<Value> = Box::new(any!({
            "field":"field",
        }));

        let path: Box<JsonPath> =
            Box::from(JsonPath::try_from("$.field[?(@ == 0)]").expect("the path is correct"));
        let v = path.find_slice(&json);
        assert_eq!(v, vec![NoValue]);
    }

    #[test]
    fn no_value_index_filter_test() {
        let json: Box<Value> = Box::new(any!({
            "field":[{"f":1},{"f":0}],
        }));

        let path: Box<JsonPath> =
            Box::from(JsonPath::try_from("$.field[?(@.f_ == 0)]").expect("the path is correct"));
        let v = path.find_slice(&json);
        assert_eq!(v, vec![NoValue]);
    }

    #[test]
    fn no_value_decent_test() {
        let json: Box<Value> = Box::new(any!({
            "field":[{"f":1},{"f":{"f_":1}}],
        }));

        let path: Box<JsonPath> =
            Box::from(JsonPath::try_from("$..f_").expect("the path is correct"));
        let v = path.find_slice(&json);
        assert_eq!(
            v,
            vec![Slice(&any!(1), "$.['field'][1].['f'].['f_']".to_string())]
        );
    }

    #[test]
    fn no_value_chain_test() {
        let json: Box<Value> = Box::new(any!({
            "field":{"field":[1]},
        }));

        let path: Box<JsonPath> =
            Box::from(JsonPath::try_from("$.field_.field").expect("the path is correct"));
        let v = path.find_slice(&json);
        assert_eq!(v, vec![NoValue]);

        let path: Box<JsonPath> = Box::from(
            JsonPath::try_from("$.field_.field[?(@ == 1)]").expect("the path is correct"),
        );
        let v = path.find_slice(&json);
        assert_eq!(v, vec![NoValue]);
    }

    #[test]
    fn no_value_filter_test() {
        // searching unexisting value returns length 0
        let json: Box<Value> = Box::new(any!([{"verb": "TEST"},{"verb": "TEST"}, {"verb": "RUN"}]));
        let path: Box<JsonPath> = Box::from(
            JsonPath::try_from("$.[?(@.verb == \"RUN1\")]").expect("the path is correct"),
        );
        let v = path.find(&json);
        let js = any!(null);
        assert_eq!(v, js);
    }

    #[test]
    fn no_value_len_test() {
        let json: Box<Value> = Box::new(any!({
            "field":{"field":1},
        }));

        let path: Box<JsonPath> =
            Box::from(JsonPath::try_from("$.field.field.length()").expect("the path is correct"));
        let v = path.find_slice(&json);
        assert_eq!(v, vec![NoValue]);

        let json: Box<Value> = Box::new(any!({
            "field":[{"a":1},{"a":1}],
        }));
        let path: Box<JsonPath> = Box::from(
            JsonPath::try_from("$.field[?(@.a == 0)].f.length()").expect("the path is correct"),
        );
        let v = path.find_slice(&json);
        assert_eq!(v, vec![NoValue]);
    }

    #[test]
    fn no_clone_api_test() {
        fn test_coercion(value: &Value) -> Value {
            value.clone()
        }

        let json: Value = serde_json::from_str(test_doc()).expect("to get json");
        let query =
            JsonPath::try_from("$..book[?(@.author size 10)].title").expect("the path is correct");

        let results = query.find_slice_ptr(&json);
        let v = results.first().expect("to get value");

        // V can be implicitly converted to &Value
        test_coercion(v);

        // To explicitly convert to &Value, use deref()
        assert_eq!(v.deref(), &any!("Sayings of the Century"));
    }

    #[test]
    fn logical_exp_test() {
        let json: Box<Value> = Box::new(any!({"first":{"second":[{"active":1},{"passive":1}]}}));

        let path: Box<JsonPath> = Box::from(
            JsonPath::try_from("$.first[?(@.does_not_exist && @.does_not_exist >= 1.0)]")
                .expect("the path is correct"),
        );
        let v = path.find_slice(&json);
        assert_eq!(v, vec![NoValue]);

        let path: Box<JsonPath> = Box::from(
            JsonPath::try_from("$.first[?(@.does_not_exist >= 1.0)]").expect("the path is correct"),
        );
        let v = path.find_slice(&json);
        assert_eq!(v, vec![NoValue]);
    }

    #[test]
    fn regex_filter_test() {
        let json: Box<Value> = Box::new(any!({
            "author":"abcd(Rees)",
        }));

        let path: Box<JsonPath> = Box::from(
            JsonPath::try_from("$.[?(@.author ~= '(?i)d\\(Rees\\)')]")
                .expect("the path is correct"),
        );
        assert_eq!(
            path.find_slice(&json.clone()),
            vec![Slice(&any!({"author":"abcd(Rees)"}), "$".to_string())]
        );
    }

    #[test]
    fn logical_not_exp_test() {
        let json: Box<Value> = Box::new(any!({"first":{"second":{"active":1}}}));
        let path: Box<JsonPath> = Box::from(
            JsonPath::try_from("$.first[?(!@.does_not_exist >= 1.0)]")
                .expect("the path is correct"),
        );
        let v = path.find_slice(&json);
        assert_eq!(
            v,
            vec![Slice(
                &any!({"second":{"active": 1}}),
                "$.['first']".to_string()
            )]
        );

        let path: Box<JsonPath> = Box::from(
            JsonPath::try_from("$.first[?(!(@.does_not_exist >= 1.0))]")
                .expect("the path is correct"),
        );
        let v = path.find_slice(&json);
        assert_eq!(
            v,
            vec![Slice(
                &any!({"second":{"active": 1}}),
                "$.['first']".to_string()
            )]
        );

        let path: Box<JsonPath> = Box::from(
            JsonPath::try_from("$.first[?(!(@.second.active == 1) || @.second.active == 1)]")
                .expect("the path is correct"),
        );
        let v = path.find_slice(&json);
        assert_eq!(
            v,
            vec![Slice(
                &any!({"second":{"active": 1}}),
                "$.['first']".to_string()
            )]
        );

        let path: Box<JsonPath> = Box::from(
            JsonPath::try_from("$.first[?(!@.second.active == 1 && !@.second.active == 1 || !@.second.active == 2)]")
                .expect("the path is correct"),
        );
        let v = path.find_slice(&json);
        assert_eq!(
            v,
            vec![Slice(
                &any!({"second":{"active": 1}}),
                "$.['first']".to_string()
            )]
        );
    }
}
