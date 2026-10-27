//! Conservative battle selection evidence. Missing/occluded UI is Unknown,
//! never evidence of selection or normal running. This module never deletes.

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SelectionState {
    Normal,
    Selected,
    Unknown,
}

#[derive(Debug, Clone, Copy)]
pub struct SelectionEvidence {
    pub state: SelectionState,
    pub normal_score: f64,
    pub selected_score: f64,
    pub menu_score: f64,
}

struct Glyph {
    x: f64,
    y: f64,
    rows: &'static [&'static str],
}

// Only tiny glyph masks are retained: # is foreground, . is background,
// spaces omit antialiased/ambiguous pixels. Coordinates are right/top anchored.
const PC_X: Glyph = Glyph { x: 557., y: 13., rows: &[
    "  .....   .....", "## ....##......", ".## ..## ......", "..##. ##.......",
    "...#### .......", "....###........", "....###........", "...## ##.......",
    ".. ##..##......", "..## .. ##.....", ". ##.... ##....", ".  ......   ...",
] };
const MOBILE_X: Glyph = Glyph { x: 535., y: 12., rows: &[
    "# .... # ...", " # .. ##....", ". # .## ....", ".. # # .....",
    "... ##......", "... ## .....", ".. #### ....", "..## .##....",
    ". # ...##...", ".##.....##..", "............",
] };
const PC_SELECTED: Glyph = Glyph { x: 547., y: 19., rows: &[
    ".........................", "..##....# ....#..........", "..####..### ..### .......",
    "..###########  #### .....", "..################### ...", ".. ###################...",
    "...##### ##### #####.....", "...### ..### ..### ......", "...##....# ....# ........",
    ".........................",
] };
const MOBILE_SELECTED: Glyph = Glyph { x: 528., y: 17., rows: &[
    "......................", ". # ...# ...  ........", ". ### .### .    ......",
    ". ##########      ....", "..############     ...", "..##### ####      ....",
    "..### . ## ..   ......", "..  ...   ...  .......", "......................",
] };

fn pixel(gray: &[u8], w: usize, h: usize, x: f64, y: f64) -> Option<f64> {
    if !x.is_finite() || !y.is_finite() || x < 0. || y < 0. || x > (w-1) as f64 || y > (h-1) as f64 { return None; }
    let (ix, iy) = (x.floor() as usize, y.floor() as usize);
    let (dx, dy) = (x-ix as f64, y-iy as f64);
    let right = (ix+1).min(w-1);
    let bottom = (iy+1).min(h-1);
    Some((gray[iy*w+ix] as f64*(1.-dx)+gray[iy*w+right] as f64*dx)*(1.-dy)
        +(gray[bottom*w+ix] as f64*(1.-dx)+gray[bottom*w+right] as f64*dx)*dy)
}

fn glyph(gray: &[u8], w: usize, h: usize, scale: f64, g: &Glyph) -> Option<(f64,f64)> {
    let mut positive = Vec::with_capacity(128);
    let (mut neg,mut count)=(0.,0usize);
    for (y,row) in g.rows.iter().enumerate() {
        for (x,kind) in row.bytes().enumerate() {
            if kind==b' ' { continue; }
            let v=pixel(gray,w,h,w as f64+(g.x+x as f64-640.)*scale,(g.y+y as f64)*scale)?;
            if kind==b'#' { positive.push(v); } else { neg+=v;count+=1; }
        }
    }
    if positive.is_empty() || count==0 { return None; }
    neg/=count as f64;
    let contrast=positive.iter().sum::<f64>()/positive.len() as f64-neg;
    let floor=neg+contrast.max(0.)*0.55;
    let coverage=positive.iter().filter(|v|**v>=floor).count() as f64/positive.len() as f64;
    Some((contrast,coverage))
}

fn lower_arrow(gray: &[u8], w: usize,h: usize,scale:f64,pc:bool)->bool {
    let (x,y,rw,rh)=if pc {(551.,27.,21,10)}else{(532.,24.,18,8)};
    let mut white=0;
    for dy in 0..rh { for dx in 0..rw {
        let Some(v)=pixel(gray,w,h,w as f64+(x+dx as f64-640.)*scale,(y+dy as f64)*scale)else{return false};
        white+=usize::from(v>=155.);
    }}
    white>=10
}

#[derive(Clone,Copy)]
struct Line { x:f64,y:f64,intercept:f64,score:f64 }

// Search long bright diagonals, not image similarity. Two separated parallel
// sides in each orientation must surround the same area. HUD and bottom cards
// are excluded. The precise map, character, menu position and direction vary.
fn menu_score(gray:&[u8],w:usize,h:usize,scale:f64)->f64 {
    let edge=|x:f64,y:f64|->bool {
        (-2..=2).any(|off| {
            let yy=y+off as f64*scale;
            let Some(v)=pixel(gray,w,h,x,yy)else{return false};
            if v<155. {return false;}
            let a=pixel(gray,w,h,x,yy-4.*scale).unwrap_or(v);
            let b=pixel(gray,w,h,x,yy+4.*scale).unwrap_or(v);
            v-(a+b)*0.5>=20.
        })
    };
    let step=(4.*scale).round().max(2.) as usize;
    let length=48.*scale;
    let left=(32.*scale).round().max(1.) as usize;
    let right=w.saturating_sub((32.*scale+length).ceil() as usize);
    let top=(42.*scale).ceil() as usize;
    let bottom=(h as f64*0.82).floor() as usize;
    if left>=right || top>=bottom {return 0.;}
    let mut pairs:[Vec<(f64,f64,f64)>;2]=[vec![],vec![]];
    for (direction,sign) in [1.,-1.].iter().enumerate() {
        for slope in [0.7,0.85,1.0,1.15].iter().map(|v|v*sign) {
            let mut lines=Vec::new();
            for y in (top..bottom).step_by(step) { for x in (left..right).step_by(step) {
                let y2=y as f64+slope*length;
                if y2<top as f64 || y2>=bottom as f64 {continue;}
                let n=(0..17).filter(|k|{
                    let dx=*k as f64*length/16.;edge(x as f64+dx,y as f64+slope*dx)
                }).count();
                let score=n as f64/17.;
                if score>=0.76 {
                    let cx=x as f64+length*0.5;
                    let cy=y as f64+slope*length*0.5;
                    lines.push(Line{x:cx,y:cy,intercept:cy-slope*(cx-w as f64*0.5),score});
                }
            }}
            // A bounded set of the strongest separated line segments.
            lines.sort_by(|a,b|b.score.total_cmp(&a.score));
            let mut unique:Vec<Line>=vec![];
            for l in lines {
                if unique.iter().any(|q|(q.intercept-l.intercept).abs()<8.*scale) {continue;}
                unique.push(l);
                if unique.len()==32 {break;}
            }
            for (i,a) in unique.iter().enumerate() {for b in &unique[i+1..] {
                let gap=(a.intercept-b.intercept).abs();
                if gap<55.*scale || gap>250.*scale || (a.x-b.x).abs()>260.*scale || (a.y-b.y).abs()>200.*scale {continue;}
                pairs[direction].push(((a.x+b.x)*0.5,(a.y+b.y)*0.5,a.score.min(b.score)));
            }}
        }
    }
    let mut best:f64=0.;
    for a in &pairs[0] { for b in &pairs[1] {
        if (a.0-b.0).abs()<70.*scale && (a.1-b.1).abs()<70.*scale {best=best.max(a.2.min(b.2));}
    }}
    best
}

/// `scale` is DetectionGeometry.scale; glyph positions share the detector's
/// top/right anchored geometry. `pc` selects the corresponding profile layout.
pub fn classify_detailed(gray:&[u8],width:usize,height:usize,scale:f64,pc:bool)->SelectionEvidence {
    let mut result=SelectionEvidence{state:SelectionState::Unknown,normal_score:0.,selected_score:0.,menu_score:0.};
    if width<100 || height<100 || width.checked_mul(height)!=Some(gray.len()) || !scale.is_finite() || !(0.35..=3.).contains(&scale) {return result;}
    let normal=glyph(gray,width,height,scale,if pc{&PC_X}else{&MOBILE_X}).unwrap_or((0.,0.));
    let selected=glyph(gray,width,height,scale,if pc{&PC_SELECTED}else{&MOBILE_SELECTED}).unwrap_or((0.,0.));
    result.normal_score=normal.0.max(0.)*normal.1;
    result.selected_score=selected.0.max(0.)*selected.1;
    let normal_pass=normal.0>=65. && normal.1>=0.78 && lower_arrow(gray,width,height,scale,pc);
    let selected_pass=selected.0>=28. && selected.1>=0.72;
    if normal_pass && !selected_pass {result.state=SelectionState::Normal;}
    else if selected_pass && !normal_pass {
        result.menu_score=menu_score(gray,width,height,scale);
        if result.menu_score>=0.76 {result.state=SelectionState::Selected;}
    }
    result
}

pub fn classify(gray:&[u8],width:usize,height:usize,scale:f64,pc:bool)->SelectionState {
    classify_detailed(gray,width,height,scale,pc).state
}

#[cfg(test)]
mod tests {
    use super::*;
    fn paint_glyph(f:&mut[u8],w:usize,g:&Glyph) {
        for(y,row)in g.rows.iter().enumerate(){for(x,b)in row.bytes().enumerate(){
            if b!=b' ' {f[(g.y as usize+y)*w+g.x as usize+x]=if b==b'#'{230}else{35};}
        }}
    }
    fn diamond(f:&mut[u8],w:usize,cx:f64,cy:f64) {
        let points=[(cx,cy-80.),(cx+100.,cy),(cx,cy+80.),(cx-100.,cy),(cx,cy-80.)];
        for ab in points.windows(2){for k in 0..400 {
            let t=k as f64/400.;let x=(ab[0].0*(1.-t)+ab[1].0*t).round()as usize;let y=(ab[0].1*(1.-t)+ab[1].1*t).round()as usize;
            f[y*w+x]=240;
        }}
    }
    #[test]
    fn missing_controls_are_unknown_including_a_bright_map() {
        for v in [0,40,160,255] {assert_eq!(classify(&vec![v;640*360],640,360,1.,true),SelectionState::Unknown);}
        assert_eq!(classify(&[0;10],640,360,1.,true),SelectionState::Unknown);
        assert_eq!(classify(&vec![0;640*360],640,360,f64::NAN,true),SelectionState::Unknown);
    }
    #[test]
    fn speed_glyph_needs_its_lower_arrow() {
        for(pc,h,g)in [(true,360,&PC_X),(false,288,&MOBILE_X)] {
            let mut f=vec![35;640*h];paint_glyph(&mut f,640,g);
            assert_eq!(classify(&f,640,h,1.,pc),SelectionState::Unknown);
            let(x,y)=if pc{(558,29)}else{(537,26)};
            for yy in y..y+3{for xx in x..x+5{f[yy*640+xx]=230;}}
            assert_eq!(classify(&f,640,h,1.,pc),SelectionState::Normal);
        }
    }
    #[test]
    fn selected_symbol_requires_independent_menu_edges() {
        let mut f=vec![35;640*360];paint_glyph(&mut f,640,&PC_SELECTED);
        assert_eq!(classify(&f,640,360,1.,true),SelectionState::Unknown);
        diamond(&mut f,640,300.,165.);
        assert_eq!(classify(&f,640,360,1.,true),SelectionState::Selected);
    }
    #[test]
    fn menu_without_control_evidence_never_authorizes_deletion() {
        let mut f=vec![35;640*360];diamond(&mut f,640,300.,165.);
        assert_eq!(classify(&f,640,360,1.,true),SelectionState::Unknown);
    }
}
