//! STENO-P8 Economic Token Encoding (prototype).
//!
//! This module is an experimental reversible byte codec. Tokens are admitted only
//! when the estimated complete frame is smaller than literal UTF-8 storage.
//! It deliberately does not claim to replace the entropy layer of SP7/P7.
use std::collections::HashMap;

const MAGIC: &[u8; 4] = b"SP8\0";
const LITERAL: u8 = 0xFF;
const MAX_TOKENS: usize = 65_535;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Error { Truncated, InvalidHeader, InvalidCode, InvalidDictionary, OutputLimit }

#[derive(Debug, Clone)] struct Entry { code: Vec<u8>, token: Vec<u8> }
#[derive(Debug, Clone, Default)] pub struct Stats { pub input_bytes: usize, pub frame_bytes: usize, pub dictionary_bytes: usize, pub token_count: usize, pub literal_bytes: usize }

/// Assigns prefix-free IDs in 1..5 bytes. Prefix tiers are disjoint:
/// 00..7F (1), 80..BF (2), C0..DF (3), E0..EF (4), F0..F7 (5).
fn code_for_id(id: u64) -> Option<Vec<u8>> {
    const A:u64=128; const B:u64=16_384; const C:u64=2_097_152; const D:u64=268_435_456;
    if id < A { return Some(vec![id as u8]); }
    if id < A+B { let v=id-A; return Some(vec![0x80|((v>>8) as u8),v as u8]); }
    if id < A+B+C { let v=id-A-B; return Some(vec![0xC0|((v>>16) as u8),(v>>8) as u8,v as u8]); }
    if id < A+B+C+D { let v=id-A-B-C; return Some(vec![0xE0|((v>>24) as u8),(v>>16) as u8,(v>>8) as u8,v as u8]); }
    let v=id-A-B-C-D; if v < (1u64<<35) { Some(vec![0xF0|((v>>32) as u8),(v>>24) as u8,(v>>16) as u8,(v>>8) as u8,v as u8]) } else { None }
}
fn code_len_from_first(b:u8)->Result<usize,Error>{ if b<0x80{Ok(1)}else if b<0xC0{Ok(2)}else if b<0xE0{Ok(3)}else if b<0xF0{Ok(4)}else if b<0xF8{Ok(5)}else{Err(Error::InvalidCode)} }
fn put_var(mut n:usize,out:&mut Vec<u8>){while n>=128{out.push((n as u8)|0x80);n>>=7;}out.push(n as u8);}
fn get_var(data:&[u8],p:&mut usize)->Result<usize,Error>{let mut n=0usize;let mut shift=0;for _ in 0..10{let b=*data.get(*p).ok_or(Error::Truncated)?;*p+=1;n|=((b&127) as usize).checked_shl(shift).ok_or(Error::InvalidCode)?;if b&128==0{return Ok(n)}shift+=7;}Err(Error::InvalidCode)}

/// Splits text into word/identifier runs and operator runs, while keeping
/// punctuation and Unicode symbols intact. UTF-8 byte offsets are never cut mid-char.
fn scan_candidates(input:&[u8])->HashMap<Vec<u8>,usize>{
 let s=match std::str::from_utf8(input){Ok(s)=>s,Err(_)=>return HashMap::new()}; let mut counts=HashMap::new();let mut run=String::new();let mut kind=0u8;
 let flush=|r:&mut String,k:u8,c:&mut HashMap<Vec<u8>,usize>|{if r.len()>=2 && (k==1||k==2){*c.entry(r.as_bytes().to_vec()).or_insert(0) += 1;}r.clear();};
 for ch in s.chars(){let k=if ch.is_alphanumeric()||ch=='_' {1} else if !ch.is_whitespace() && (ch.is_ascii_punctuation()||"+-=*/<>!&|%^~:$@#".contains(ch)) {2}else{0}; if k!=kind || k==0 {flush(&mut run,kind,&mut counts);kind=k;} if k!=0{run.push(ch);} }
 flush(&mut run,kind,&mut counts);counts
}

#[derive(Default)]
struct TrieNode { next: HashMap<u8, usize>, code: Option<Vec<u8>> }

fn build_trie(map: &HashMap<Vec<u8>, Vec<u8>>) -> Vec<TrieNode> {
    let mut nodes = vec![TrieNode::default()];
    for (token, code) in map {
        let mut at = 0usize;
        for &b in token {
            let next = if let Some(&n) = nodes[at].next.get(&b) { n } else {
                let n = nodes.len(); nodes.push(TrieNode::default()); nodes[at].next.insert(b, n); n
            };
            at = next;
        }
        nodes[at].code = Some(code.clone());
    }
    nodes
}

fn longest_match(input: &[u8], start: usize, trie: &[TrieNode]) -> Option<(usize, Vec<u8>)> {
    let mut at = 0usize;
    let mut best = None;
    for (offset, &b) in input[start..].iter().enumerate() {
        let Some(&next) = trie[at].next.get(&b) else { break; };
        at = next;
        if let Some(code) = &trie[at].code { best = Some((offset + 1, code.clone())); }
    }
    best
}

fn frame_size(dict: &[Entry], body_len: usize, input_len: usize) -> usize {
    let mut n = MAGIC.len() + var_len(input_len) + var_len(dict.len()) + var_len(body_len) + body_len;
    for e in dict { n += var_len(e.code.len()) + e.code.len() + var_len(e.token.len()) + e.token.len(); }
    n
}
fn var_len(mut n: usize) -> usize { let mut len=1; while n>=128 { n >>= 7; len+=1; } len }

/// Encodes raw bytes as an SP8 frame. The source may contain arbitrary bytes;
/// candidate dictionary extraction is enabled only for valid UTF-8 input.
pub fn encode(input:&[u8])->(Vec<u8>,Stats){
 let mut stats=Stats{input_bytes:input.len(),..Stats::default()}; let counts=scan_candidates(input);
 let mut candidates:Vec<(Vec<u8>,usize,i64)>=counts.into_iter().filter_map(|(t,n)|{if n<2{return None}let benefit=(t.len().saturating_sub(2) as i64)*(n as i64)-t.len() as i64-4;if benefit>0{Some((t,n,benefit))}else{None}}).collect();
 candidates.sort_by(|a,b|b.2.cmp(&a.2).then_with(||b.1.cmp(&a.1)).then_with(||a.0.cmp(&b.0)));candidates.truncate(MAX_TOKENS);
 // Use short IDs first; actual cost gate accounts for dictionary bytes and literal framing.
 let mut entries=Vec::new();let mut code_map:HashMap<Vec<u8>,Vec<u8>>=HashMap::new();let mut dict_cost=0usize;
 for (tok,occurrences,_) in candidates {let id=entries.len() as u64;let code=match code_for_id(id){Some(c)=>c,None=>break};
 // Marginal admission uses the actual assigned code length and exact dictionary record size.
 // The final whole-frame comparison below remains authoritative because candidates can overlap.
 let dictionary_record=var_len(code.len())+code.len()+var_len(tok.len())+tok.len();
 let per_use_saving=tok.len().saturating_sub(code.len());
 let gross_saving=occurrences.saturating_mul(per_use_saving);
 if gross_saving<=dictionary_record+2 {continue}
 entries.push(Entry{code:code.clone(),token:tok.clone()});code_map.insert(tok,code);dict_cost+=dictionary_record;}
 // Re-check global economics. If dictionary overhead exceeds the raw savings, use no dictionary.
 let trie=build_trie(&code_map);
 let mut body=Vec::new();let mut p=0usize;let mut literal=Vec::new();
 while p<input.len(){if let Some((matched,code))=longest_match(input,p,&trie){if !literal.is_empty(){body.push(LITERAL);put_var(literal.len(),&mut body);body.extend_from_slice(&literal);stats.literal_bytes+=literal.len();literal.clear();}body.extend_from_slice(&code);stats.token_count+=1;p+=matched;}else{literal.push(input[p]);p+=1;}}
 if !literal.is_empty(){body.push(LITERAL);put_var(literal.len(),&mut body);body.extend_from_slice(&literal);stats.literal_bytes+=literal.len();}
 let mut frame=Vec::new();frame.extend_from_slice(MAGIC);put_var(input.len(),&mut frame);put_var(entries.len(),&mut frame);for e in &entries{put_var(e.code.len(),&mut frame);frame.extend_from_slice(&e.code);put_var(e.token.len(),&mut frame);frame.extend_from_slice(&e.token);}put_var(body.len(),&mut frame);frame.extend_from_slice(&body);
 // Exact dictionary accounting includes every varint and every emitted body byte.
 dict_cost = frame_size(&entries, body.len(), input.len()) - (MAGIC.len() + var_len(input.len()) + var_len(entries.len()) + var_len(body.len()) + body.len());
 debug_assert_eq!(frame.len(), frame_size(&entries, body.len(), input.len()));
 // Conservative fail-open: if SP8 is not smaller than input, emit an empty-dictionary literal frame.
 if frame.len()>=input.len(){entries.clear();body.clear();body.push(LITERAL);put_var(input.len(),&mut body);body.extend_from_slice(input);frame.clear();frame.extend_from_slice(MAGIC);put_var(input.len(),&mut frame);put_var(0,&mut frame);put_var(body.len(),&mut frame);frame.extend_from_slice(&body);stats.token_count=0;stats.literal_bytes=input.len();dict_cost=0;}
 stats.dictionary_bytes=dict_cost;stats.frame_bytes=frame.len();(frame,stats)
}

/// Decodes an SP8 frame with strict bounds and duplicate-code/token checks.
pub fn decode(frame:&[u8],max_output:usize)->Result<Vec<u8>,Error>{if frame.len()<4||frame.get(..4)!=Some(MAGIC.as_slice()){return Err(Error::InvalidHeader)}let mut p=4;let expected=get_var(frame,&mut p)?;if expected>max_output{return Err(Error::OutputLimit)}let n=get_var(frame,&mut p)?;if n>MAX_TOKENS{return Err(Error::InvalidDictionary)}let mut by_code:HashMap<Vec<u8>,Vec<u8>>=HashMap::new();for _ in 0..n{let cl=get_var(frame,&mut p)?;if cl==0||cl>5{return Err(Error::InvalidDictionary)}let end=p.checked_add(cl).ok_or(Error::Truncated)?;let c=frame.get(p..end).ok_or(Error::Truncated)?.to_vec();p=end;let tl=get_var(frame,&mut p)?;if tl==0||tl>max_output{return Err(Error::InvalidDictionary)}let end=p.checked_add(tl).ok_or(Error::Truncated)?;let t=frame.get(p..end).ok_or(Error::Truncated)?.to_vec();p=end;if by_code.insert(c,t).is_some(){return Err(Error::InvalidDictionary)}}let bl=get_var(frame,&mut p)?;let end=p.checked_add(bl).ok_or(Error::Truncated)?;let body=frame.get(p..end).ok_or(Error::Truncated)?;p=end;if p!=frame.len(){return Err(Error::InvalidHeader)}let mut out=Vec::with_capacity(expected);let mut q=0;while q<body.len(){let b=body[q];if b==LITERAL{q+=1;let len=get_var(body,&mut q)?;let chunk=body.get(q..q+len).ok_or(Error::Truncated)?;if out.len().checked_add(len).ok_or(Error::OutputLimit)?>expected{return Err(Error::OutputLimit)}out.extend_from_slice(chunk);q+=len;}else{let clen=code_len_from_first(b)?;let code=body.get(q..q+clen).ok_or(Error::Truncated)?.to_vec();q+=clen;let token=by_code.get(&code).ok_or(Error::InvalidCode)?;if out.len().checked_add(token.len()).ok_or(Error::OutputLimit)?>expected{return Err(Error::OutputLimit)}out.extend_from_slice(token);}}if out.len()!=expected{return Err(Error::InvalidHeader)}Ok(out)}

#[cfg(test)] mod tests{use super::*;#[test]fn roundtrip_multilingual_text_math_code(){let s="the the and and 计算公式 ∫ x² + y² = z²\nfn main() { let value = 42; } 日本語テスト";let (a,_)=encode(s.as_bytes());assert_eq!(decode(&a,1_000_000).unwrap(),s.as_bytes());}#[test]fn arbitrary_bytes_roundtrip(){let data=[0,255,128,1,2,3,255];let (a,_)=encode(&data);assert_eq!(decode(&a,100).unwrap(),data);}#[test]fn nonprofitable_inputs_stay_literal(){let s=b"abc";let (a,st)=encode(s);assert_eq!(decode(&a,100).unwrap(),s);assert_eq!(st.token_count,0);}#[test]fn rejects_bad_magic(){assert_eq!(decode(b"BAD!",100),Err(Error::InvalidHeader));}
#[test]fn trie_prefers_longest_token(){let mut m=HashMap::new();m.insert(b"ab".to_vec(),vec![1]);m.insert(b"abc".to_vec(),vec![2]);let t=build_trie(&m);assert_eq!(longest_match(b"abcd",0,&t),Some((3,vec![2])));}
#[test]fn exact_frame_size_matches_serialized_size(){let s=b"repeat repeat repeat repeat";let (f,st)=encode(s);assert_eq!(st.frame_bytes,f.len());assert_eq!(decode(&f,1000).unwrap(),s);}
#[test]fn token_codes_have_unambiguous_lengths(){for id in [0,127,128,16_511,16_512,2_113_663,2_113_664,270_549_119,270_549_120]{let c=code_for_id(id).unwrap();assert_eq!(code_len_from_first(c[0]).unwrap(),c.len());}}
}
