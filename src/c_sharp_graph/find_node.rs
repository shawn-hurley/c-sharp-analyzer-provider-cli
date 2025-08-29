use anyhow::Error;
use anyhow::Ok;
use stack_graphs::storage::SQLiteReader;
use std::path::PathBuf;
use std::vec;

use crate::c_sharp_graph::query::Querier;
use crate::c_sharp_graph::query::Query;
use crate::c_sharp_graph::results;

pub struct FindNode {
    #[allow(dead_code)]
    pub node_type: Option<String>,
    pub regex: String,
}

impl FindNode {
    pub fn run(self, db_path: &PathBuf) -> anyhow::Result<Vec<results::Result>, anyhow::Error> {
        println!("running search");
        let mut db = SQLiteReader::open(db_path)?;

        let paths = Self::get_file_strings(&mut db)?;
        println!("paths: {:?}", paths);

        for path in paths {
            let _ = db.load_graph_for_file(path.as_str())?;
        }
        let (graph, _, _) = db.get();

        let mut q = Querier::get_query(graph);

        q.query(self.regex)
    }

    fn get_file_strings(db: &mut SQLiteReader) -> anyhow::Result<Vec<String>, Error> {
        let mut file_strings: Vec<String> = vec![];
        let mut files = db.list_all()?;
        for file in files.try_iter()? {
            let entry = file?;
            let file_path = entry.path.into_os_string().into_string().unwrap();
            file_strings.push(file_path);
        }
        Ok(file_strings)
    }
}
