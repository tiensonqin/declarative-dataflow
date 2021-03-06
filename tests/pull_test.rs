use std::collections::HashSet;
use std::iter::FromIterator;
use std::sync::mpsc::channel;
use std::time::Duration;

use timely::dataflow::channels::pact::Pipeline;
use timely::dataflow::operators::Operator;

use declarative_dataflow::plan::{Implementable, PullLevel};
use declarative_dataflow::server::Server;
use declarative_dataflow::timestamp::Time;
use declarative_dataflow::{Aid, Datom, Plan, Rule, Value};
use declarative_dataflow::{AttributeConfig, IndexDirection, QuerySupport};
use Value::{Bool, Eid, Number, String};

struct Case {
    description: &'static str,
    plan: Plan<Aid>,
    transactions: Vec<Vec<Datom<Aid>>>,
    expectations: Vec<Vec<(Vec<Value>, u64, isize)>>,
}

fn run_cases(mut cases: Vec<Case>) {
    for case in cases.drain(..) {
        timely::execute_directly(move |worker| {
            let mut server = Server::<Aid, u64, u64>::new(Default::default());
            let (send_results, results) = channel();

            dbg!(case.description);

            let mut deps = case.plan.dependencies();
            let plan = case.plan.clone();

            dbg!(&plan);

            for tx in case.transactions.iter() {
                for datum in tx {
                    deps.attributes.insert(datum.1.clone());
                }
            }

            worker.dataflow::<u64, _, _>(|scope| {
                for dep in deps.attributes.iter() {
                    let config = AttributeConfig {
                        trace_slack: Some(Time::TxId(1)),
                        // @TODO Forward delta should be enough eventually
                        query_support: QuerySupport::AdaptiveWCO,
                        index_direction: IndexDirection::Both,
                        // query_support: QuerySupport::Delta,
                        // index_direction: IndexDirection::Forward,
                        ..Default::default()
                    };

                    server.create_attribute(scope, dep.clone(), config).unwrap();
                }

                server
                    .test_single(scope, Rule::named("query", plan))
                    .inner
                    .sink(Pipeline, "Results", move |input| {
                        input.for_each(|_time, data| {
                            for datum in data.iter() {
                                send_results.send(datum.clone()).unwrap()
                            }
                        });
                    });
            });

            let mut transactions = case.transactions.clone();
            let mut next_tx = 0;

            for (tx_id, tx_data) in transactions.drain(..).enumerate() {
                next_tx += 1;

                server.transact(tx_data, 0, 0).unwrap();
                server.advance_domain(None, next_tx).unwrap();

                worker.step_while(|| server.is_any_outdated());

                let mut expected: HashSet<(Vec<Value>, u64, isize)> =
                    HashSet::from_iter(case.expectations[tx_id].iter().cloned());

                for _i in 0..expected.len() {
                    match results.recv_timeout(Duration::from_millis(400)) {
                        Err(_err) => {
                            panic!("No result.");
                        }
                        Ok(result) => {
                            if !expected.remove(&result) {
                                panic!("Unknown result {:?}.", result);
                            }
                        }
                    }
                }

                match results.recv_timeout(Duration::from_millis(400)) {
                    Err(_err) => {}
                    Ok(result) => {
                        panic!("Extraneous result {:?}", result);
                    }
                }
            }
        });
    }
}

#[test]
fn pull_level() {
    run_cases(vec![Case {
        description: "[:find (pull ?e [:name :age]) :where [?e :admin? false]]",
        plan: Plan::PullLevel(PullLevel {
            variables: vec![],
            pull_variable: 0,
            plan: Box::new(Plan::match_av(0, "admin?", Bool(false))),
            pull_attributes: vec!["name".to_string(), "age".to_string()],
            path_attributes: vec![],
            cardinality_many: false,
        }),
        transactions: vec![vec![
            Datom::add(100, "admin?", Bool(true)),
            Datom::add(200, "admin?", Bool(false)),
            Datom::add(300, "admin?", Bool(false)),
            Datom::add(100, "name", String("Mabel".to_string())),
            Datom::add(200, "name", String("Dipper".to_string())),
            Datom::add(300, "name", String("Soos".to_string())),
            Datom::add(100, "age", Number(12)),
            Datom::add(200, "age", Number(13)),
        ]],
        expectations: vec![vec![
            (vec![Eid(200), Value::aid("age"), Number(13)], 0, 1),
            (
                vec![Eid(200), Value::aid("name"), String("Dipper".to_string())],
                0,
                1,
            ),
            (
                vec![Eid(300), Value::aid("name"), String("Soos".to_string())],
                0,
                1,
            ),
        ]],
    }]);
}

#[cfg(feature = "graphql")]
#[test]
#[rustfmt::skip]
fn graph_ql() {
    use declarative_dataflow::plan::GraphQl;
    use declarative_dataflow::binding::Binding;

    let transactions = vec![vec![
        Datom::add(100, "name", Value::from("Alice")),
        Datom::add(100, "hero", Bool(true)),
        Datom::add(200, "name", Value::from("Bob")),
        Datom::add(200, "hero", Bool(true)),
        Datom::add(300, "name", Value::from("Mabel")),
        Datom::add(300, "hero", Bool(true)),
        Datom::add(400, "name", Value::from("Dipper")),
        Datom::add(400, "hero", Bool(true)),
        
        Datom::add(300, "bested", Eid(400)),
        Datom::add(200, "bested", Eid(100)),

        Datom::add(300, "age", Number(13)),
        Datom::add(400, "age", Number(12)),
    ]];

    // We want to pull all entities carrying the `hero` attribute.
    let root_plan = declarative_dataflow::q(vec![0], vec![
        // <- arbitrary symbol here to fake a placeholder
        Binding::attribute(0, "hero", 11111),
    ]);

    run_cases(vec![
        {
            let q = "{name age height mass}";

            let expectations = vec![vec![
                (vec![Eid(100), Value::aid("name"), Value::from("Alice")], 0, 1),
                (vec![Eid(200), Value::aid("name"), Value::from("Bob")], 0, 1),
                (vec![Eid(300), Value::aid("name"), Value::from("Mabel")], 0, 1),
                (vec![Eid(400), Value::aid("name"), Value::from("Dipper")], 0, 1),
                (vec![Eid(300), Value::aid("age"), Number(13)], 0, 1),
                (vec![Eid(400), Value::aid("age"), Number(12)], 0, 1),
            ]];
                
            Case {
                description: q,
                plan: Plan::GraphQl(GraphQl::with_plan(root_plan.clone(), q.to_string())),
                transactions: transactions.clone(),
                expectations,
            }
        },
        {
            let q = "{name bested { name }}";
            
            let expectations = vec![vec![
                (vec![Eid(100), Value::aid("name"), Value::from("Alice")], 0, 1),
                (vec![Eid(200), Value::aid("name"), Value::from("Bob")], 0, 1),
                (vec![Eid(300), Value::aid("name"), Value::from("Mabel")], 0, 1),
                (vec![Eid(400), Value::aid("name"), Value::from("Dipper")], 0, 1),
                (vec![Eid(300), Value::aid("bested"), Eid(400), Value::aid("name"), Value::from("Dipper")], 0, 1),
                (vec![Eid(200), Value::aid("bested"), Eid(100), Value::aid("name"), Value::from("Alice")], 0, 1),
            ]];
            
            Case {
                description: q,
                plan: Plan::GraphQl(GraphQl::with_plan(root_plan.clone(), q.to_string())),
                transactions: transactions.clone(),
                expectations,
            }
        },
        {
            let q = "{bested(name: \"Dipper\") { age }}";

            let expectations = vec![vec![
                (vec![Eid(300), Value::aid("bested"), Eid(400), Value::aid("age"), Number(12)], 0, 1),
                (vec![Eid(200), Value::aid("bested"), Eid(100), Value::aid("db__id"), Eid(100)], 0, 1),
                (vec![Eid(300), Value::aid("bested"), Eid(400), Value::aid("db__id"), Eid(400)], 0, 1),
            ]];

            Case {
                description: q,
                plan: Plan::GraphQl(GraphQl::with_plan(root_plan.clone(), q.to_string())),
                transactions: transactions.clone(),
                expectations,
            }
        },
        {
            let q = "{age bested(name: \"Dipper\") { age }}";

            let expectations = vec![vec![
                (vec![Eid(300), Value::aid("age"), Number(13)], 0, 1),
                (vec![Eid(300), Value::aid("bested"), Eid(400), Value::aid("age"), Number(12)], 0, 1),
                (vec![Eid(400), Value::aid("age"), Number(12)], 0, 1),
            ]];

            Case {
                description: q,
                plan: Plan::GraphQl(GraphQl::with_plan(root_plan.clone(), q.to_string())),
                transactions: transactions.clone(),
                expectations,
            }
        }
    ]);
}
