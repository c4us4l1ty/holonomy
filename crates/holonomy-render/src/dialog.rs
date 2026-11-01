//! Native modal dialogs for the desktop driver. No terminal prompt and no toolkit dependency.
//! Secrets are masked by the driver before a view reaches this module.
use crate::tree::{Label, Node, Rect, RoundRect, Style, SurfaceTree};
use crate::{DamageRect, Layout};

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field { pub label: String, pub value: String, pub secret: bool }
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DialogView {
    pub title: String, pub message: String, pub fields: Vec<Field>, pub focus: usize,
    pub error: Option<String>, pub confirm: String, pub cancel: Option<String>, pub choices: Vec<String>, pub choice: usize,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Hit { Field(usize), Confirm, Cancel, Choice(usize), None }
#[derive(Debug, Clone, Copy)]
pub struct Geometry { pub panel: DamageRect, pub fields: usize, pub row_h: u32, pub confirm: DamageRect, pub cancel: DamageRect }
pub fn geometry(l: &Layout, d: &DialogView) -> Geometry {
    let w = l.width.min(620).max(280);
    let row_h = 52;
    let h = (150 + (d.fields.len() + d.choices.len().min(6)) as u32 * row_h + if d.error.is_some() { 28 } else { 0 }).min(l.height.saturating_sub(16)).max(180);
    let x = (l.width.saturating_sub(w)) / 2;
    let y = (l.height.saturating_sub(h)) / 2;
    let panel = DamageRect::new(x,y,w,h);
    let by = y + h - 52;
    let cancel = DamageRect::new(x + w - 224, by, 100, 36);
    let confirm = DamageRect::new(x + w - 116, by, 100, 36);
    Geometry { panel, fields: d.fields.len(), row_h, confirm, cancel }
}
pub fn hit(l: &Layout, d: &DialogView, x: i32, y: i32) -> Hit {
    let g = geometry(l,d);
    if g.confirm.contains(x,y) { return Hit::Confirm; }
    if d.cancel.is_some() && g.cancel.contains(x,y) { return Hit::Cancel; }
    for i in 0..g.fields {
        let r = DamageRect::new(g.panel.x+20,g.panel.y+92+i as u32*g.row_h,g.panel.width-40,34);
        if r.contains(x,y) { return Hit::Field(i); }
    }
    let start = g.panel.y + 92 + d.fields.len() as u32 * g.row_h;
    for i in 0..d.choices.len().min(6) {
        let r = DamageRect::new(g.panel.x+20,start+i as u32*g.row_h,g.panel.width-40,34);
        if r.contains(x,y) { return Hit::Choice(i); }
    }
    Hit::None
}
fn label(out:&mut Vec<Node>, x:u32,y:u32,s:&str,colour:u32) {
    out.push(SurfaceTree::leaf(Node::Label(Label::new(x as i32,y as i32,s,Style::REGULAR,0,colour))));
}
pub fn push_tree(tree:&mut SurfaceTree,l:&Layout,d:&DialogView) {
    let g=geometry(l,d); let p=g.panel;
    tree.before.push(SurfaceTree::leaf(Node::Rect(Rect::new(0,0,l.width,l.height,0xA0000000))));
    tree.before.push(SurfaceTree::leaf(Node::RoundRect(RoundRect::new(p.x as i32,p.y as i32,p.width,p.height,12,0xFF20242B))));
    label(&mut tree.before,p.x+20,p.y+18,&d.title,0xFFF2F4F8);
    label(&mut tree.before,p.x+20,p.y+48,&d.message,0xFFB8C0CC);
    for (i,f) in d.fields.iter().enumerate() {
        let y=p.y+76+i as u32*g.row_h;
        label(&mut tree.before,p.x+20,y,&f.label,0xFFD0D6E0);
        let colour=if i==d.focus {0xFF7AA2F7} else {0xFF4A5361};
        tree.before.push(SurfaceTree::leaf(Node::Rect(Rect::new(p.x as i32+20,y as i32+16,p.width-40,34,colour))));
        tree.before.push(SurfaceTree::leaf(Node::Rect(Rect::new(p.x as i32+21,y as i32+17,p.width-42,32,0xFF11151B))));
        let value=if f.secret {"•".repeat(f.value.chars().count())} else {f.value.clone()};
        label(&mut tree.before,p.x+28,y+24,&value,0xFFF2F4F8);
        if i==d.focus { let cx=p.x+28+value.chars().count() as u32*8; tree.before.push(SurfaceTree::leaf(Node::Rect(Rect::new(cx as i32,y as i32+22,2,18,0xFF7AA2F7)))); }
    }
    let start=p.y+92+d.fields.len() as u32*g.row_h;
    for i in 0..d.choices.len().min(6) {
        let y=start+i as u32*g.row_h;
        let colour=if i==d.choice {0xFF3B6FD4} else {0xFF3A424E};
        tree.before.push(SurfaceTree::leaf(Node::Rect(Rect::new(p.x as i32+20,y as i32,p.width-40,34,colour))));
        label(&mut tree.before,p.x+28,y+8,&d.choices[i],0xFFFFFFFF);
    }
    if let Some(e)=&d.error { label(&mut tree.before,p.x+20,p.y+p.height-82,e,0xFFFF8A8A); }
    for (r,text,primary) in [(g.confirm,d.confirm.as_str(),true),(g.cancel,d.cancel.as_deref().unwrap_or(""),false)] {
        if text.is_empty() {continue;}
        tree.before.push(SurfaceTree::leaf(Node::RoundRect(RoundRect::new(r.x as i32,r.y as i32,r.width,r.height,7,if primary {0xFF3B6FD4} else {0xFF3A424E}))));
        label(&mut tree.before,r.x+12,r.y+10,text,0xFFFFFFFF);
    }
}
