#[derive(Debug)]
pub struct Module {
    pub name: String,
    pub items: Vec<()>,
}

#[derive(Debug)]
pub struct Interface {
    pub name: String,
    pub items: Vec<()>,
}

#[derive(Debug)]
pub enum Item {
    Inst(InstItem),
}

#[derive(Debug)]
pub struct InstItem {
    pub target: String,
    pub name: String,
    pub params: Vec<()>,
}
