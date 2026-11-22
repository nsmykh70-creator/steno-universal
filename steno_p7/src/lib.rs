//! STENO-P7: lossless multilingual text transform and entropy-coded archive.
//! SP7 v2 adds batched literals, strict decoding limits, rANS and adaptive range entropy modes.
use std::collections::{HashMap, HashSet};

pub mod stage7;
pub mod economic;
pub mod math;
pub mod code;
pub mod joint;
pub mod integrity;
pub mod stage20;

const MAGIC: &[u8; 4] = b"SP7\0";
const VERSION_V1: u8 = 1;
const VERSION_V2: u8 = 2;
const ESCAPE: u8 = 0xF8;
const RANS_SCALE: u32 = 12;
const RANS_TOTAL: u32 = 1 << RANS_SCALE;
const RANS_L: u32 = 1 << 23;
const CONTEXT_COUNT: usize = 21;
const ADAPTIVE_CONTEXT_COUNT: usize = 32;
const CONTEXT_TABLE_BYTES: usize = CONTEXT_COUNT * 256 * 2;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodecError {
    InvalidHeader, UnsupportedVersion, Truncated, InvalidToken, InvalidUtf8,
    DictionaryTooLarge, OutputTooLarge, ArchiveTooLarge, InvalidDictionary,
    InvalidEntropyStream, IntegerOverflow,
}
impl std::fmt::Display for CodecError { fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { write!(f, "{self:?}") } }
impl std::error::Error for CodecError {}

#[derive(Debug, Clone)]
pub struct CodecLimits {
    pub max_archive_bytes: usize,
    pub max_output_bytes: usize,
    pub max_dictionary_entries: usize,
    pub max_dictionary_entry_bytes: usize,
    pub max_stream_bytes: usize,
}
impl Default for CodecLimits {
    fn default() -> Self { Self { max_archive_bytes: 512*1024*1024, max_output_bytes: 1024*1024*1024, max_dictionary_entries: 50_000, max_dictionary_entry_bytes: 1*1024*1024, max_stream_bytes: 512*1024*1024 } }
}

/// Prefix-tier IDs: one through five bytes, with disjoint numeric ranges.
pub mod token_id {
    use super::CodecError;
    const R1:u64=1<<7; const R2:u64=1<<14; const R3:u64=1<<21; const R4:u64=1<<28; const R5:u64=1<<35;
    pub fn encoded_len(id:u64)->Option<usize>{ if id<R1{Some(1)}else if id<R1+R2{Some(2)}else if id<R1+R2+R3{Some(3)}else if id<R1+R2+R3+R4{Some(4)}else if id<R1+R2+R3+R4+R5{Some(5)}else{None} }
    pub fn encode(id:u64,out:&mut Vec<u8>)->Result<(),CodecError>{
        let n=encoded_len(id).ok_or(CodecError::InvalidToken)?;
        match n { 1=>out.push(id as u8), 2=>{let v=id-R1;out.push(0x80|((v>>8)as u8));out.push(v as u8)}, 3=>{let v=id-R1-R2;out.push(0xC0|((v>>16)as u8));out.extend_from_slice(&(v as u16).to_be_bytes())}, 4=>{let v=id-R1-R2-R3;out.push(0xE0|((v>>24)as u8));out.extend_from_slice(&(v as u32).to_be_bytes())}, 5=>{let v=id-R1-R2-R3-R4;out.push(0xF0|((v>>32)as u8));out.extend_from_slice(&(v as u32).to_be_bytes())}, _=>unreachable!() } Ok(())
    }
    pub fn decode(input:&[u8],pos:&mut usize)->Result<u64,CodecError>{
        let b0=*input.get(*pos).ok_or(CodecError::Truncated)?; *pos=pos.checked_add(1).ok_or(CodecError::IntegerOverflow)?;
        if b0<0x80{return Ok(b0 as u64)}
        let (len,bits,base)=if b0<0xC0{(2,6,R1)}else if b0<0xE0{(3,5,R1+R2)}else if b0<0xF0{(4,4,R1+R2+R3)}else if b0<0xF8{(5,3,R1+R2+R3+R4)}else{return Err(CodecError::InvalidToken)};
        let end=pos.checked_add(len-1).ok_or(CodecError::IntegerOverflow)?;
        let tail=input.get(*pos..end).ok_or(CodecError::Truncated)?; *pos=end;
        let mut value=(b0&((1u8<<bits)-1))as u64; for b in tail{value=(value<<8)|(*b as u64)}
        let id=base.checked_add(value).ok_or(CodecError::IntegerOverflow)?;
        if encoded_len(id)!=Some(len){return Err(CodecError::InvalidToken)}
        Ok(id)
    }
}

#[derive(Debug,Clone)] struct Candidate{ text:String, score:i64 }
#[derive(Debug,Clone)] pub struct StenoP7 { min_piece_chars:usize, max_piece_chars:usize, max_dict_entries:usize, limits:CodecLimits, use_rans:bool }
impl Default for StenoP7 { fn default()->Self{Self{min_piece_chars:2,max_piece_chars:16,max_dict_entries:4096,limits:CodecLimits::default(),use_rans:true}} }
impl StenoP7 {
    pub fn new()->Self{Self::default()}
    pub fn with_limits(mut self,limits:CodecLimits)->Self{self.max_dict_entries=self.max_dict_entries.min(limits.max_dictionary_entries);self.limits=limits;self}
    pub fn set_rans_enabled(mut self,enabled:bool)->Self{self.use_rans=enabled;self}
    pub fn compress(&self,text:&str)->Vec<u8>{ self.try_compress(text).expect("input exceeds configured STENO-P7 limits") }
    pub fn try_compress(&self,text:&str)->Result<Vec<u8>,CodecError>{
        if text.len()>self.limits.max_output_bytes{return Err(CodecError::OutputTooLarge)}
        // JTCO chooses dictionary entries by measuring the complete serialized
        // representation after every proposed addition (dictionary + token stream
        // + optional entropy layer), not by frequency alone.
        let dict=self.optimize_dictionary(text)?;
        let ids:HashMap<&str,u64>=dict.iter().enumerate().map(|(i,s)|(s.as_str(),i as u64)).collect();
        let mut stream=self.best_parse_stream(text,&ids)?;
        if stream.len()>self.limits.max_stream_bytes{return Err(CodecError::ArchiveTooLarge)}
        let mut mode=0u8;
        let mut payload=stream.clone();
        let mut static_freq:Option<[u16;256]>=None;
        let mut context_freq:Option<[[u16;256];CONTEXT_COUNT]>=None;
        if self.use_rans&&!stream.is_empty(){
            let (encoded,f)=rans_encode(&stream);
            if encoded.len().checked_add(512).map_or(false,|v|v<payload.len()){
                mode=1;payload=encoded;static_freq=Some(f);
            }
            let (context_encoded,context_tables)=rans_context_encode(&stream);
            let current_cost=payload.len()+if mode==1{512}else{0};
            if context_encoded.len().checked_add(CONTEXT_TABLE_BYTES).map_or(false,|v|v<current_cost){
                mode=2;payload=context_encoded;static_freq=None;context_freq=Some(context_tables);
            }
            let adaptive_payload=adaptive_range_encode(&stream);
            let selected_cost=payload.len()+if mode==1{512}else if mode==2{CONTEXT_TABLE_BYTES}else{0};
            if adaptive_payload.len()<selected_cost{
                mode=3;payload=adaptive_payload;static_freq=None;context_freq=None;
            }
        }
        let mut out=Vec::new();out.extend_from_slice(MAGIC);out.push(VERSION_V2);put_u32(&mut out,u32::try_from(dict.len()).map_err(|_|CodecError::DictionaryTooLarge)?);
        for s in &dict{if s.is_empty()||s.len()>self.limits.max_dictionary_entry_bytes{return Err(CodecError::InvalidDictionary)}put_u32(&mut out,u32::try_from(s.len()).map_err(|_|CodecError::InvalidDictionary)?);out.extend_from_slice(s.as_bytes());}
        put_u64(&mut out,stream.len() as u64);out.push(mode);
        if let Some(f)=static_freq{for x in f{put_u16(&mut out,x)}}
        if let Some(tables)=context_freq{for table in tables{for x in table{put_u16(&mut out,x)}}}
        put_u64(&mut out,payload.len() as u64);out.extend_from_slice(&payload);
        if out.len()>self.limits.max_archive_bytes{return Err(CodecError::ArchiveTooLarge)} Ok(out)
    }
    pub fn decompress(&self,data:&[u8])->Result<String,CodecError>{
        if data.len()>self.limits.max_archive_bytes{return Err(CodecError::ArchiveTooLarge)}
        let mut p=0usize;if data.get(..4)!=Some(MAGIC){return Err(CodecError::InvalidHeader)}p=4;
        let version=*data.get(p).ok_or(CodecError::Truncated)?;p+=1;
        if version==VERSION_V1{return self.decompress_v1(data,p)}
        if version!=VERSION_V2{return Err(CodecError::UnsupportedVersion)}
        let n=read_u32(data,&mut p)? as usize;if n>self.limits.max_dictionary_entries{return Err(CodecError::DictionaryTooLarge)}
        if n>data.len().saturating_sub(p)/4{return Err(CodecError::Truncated)}
        let mut dict=Vec::with_capacity(n);let mut seen=HashSet::with_capacity(n);
        for _ in 0..n{let len=read_u32(data,&mut p)? as usize;if len==0||len>self.limits.max_dictionary_entry_bytes{return Err(CodecError::InvalidDictionary)}let end=p.checked_add(len).ok_or(CodecError::IntegerOverflow)?;let b=data.get(p..end).ok_or(CodecError::Truncated)?;p=end;let s=std::str::from_utf8(b).map_err(|_|CodecError::InvalidUtf8)?.to_owned();if !seen.insert(s.clone()){return Err(CodecError::InvalidDictionary)}dict.push(s);}
        let raw_len=usize::try_from(read_u64(data,&mut p)?).map_err(|_|CodecError::IntegerOverflow)?;if raw_len>self.limits.max_stream_bytes{return Err(CodecError::ArchiveTooLarge)}
        let mode=*data.get(p).ok_or(CodecError::Truncated)?;p+=1;
        let stream=if mode==0{let len=usize::try_from(read_u64(data,&mut p)?).map_err(|_|CodecError::IntegerOverflow)?;if len!=raw_len{return Err(CodecError::InvalidEntropyStream)}let end=p.checked_add(len).ok_or(CodecError::IntegerOverflow)?;let b=data.get(p..end).ok_or(CodecError::Truncated)?.to_vec();p=end;b}
        else if mode==1{let mut f=[0u16;256];let mut sum=0u32;for x in &mut f{*x=read_u16(data,&mut p)?;sum=sum.checked_add(*x as u32).ok_or(CodecError::IntegerOverflow)?;}if sum!=RANS_TOTAL{return Err(CodecError::InvalidEntropyStream)}let len=usize::try_from(read_u64(data,&mut p)?).map_err(|_|CodecError::IntegerOverflow)?;let end=p.checked_add(len).ok_or(CodecError::IntegerOverflow)?;let payload=data.get(p..end).ok_or(CodecError::Truncated)?;p=end;rans_decode(payload,raw_len,&f)?}
        else if mode==2{let mut tables=[[0u16;256];CONTEXT_COUNT];for table in &mut tables{let mut sum=0u32;for x in table{*x=read_u16(data,&mut p)?;sum=sum.checked_add(*x as u32).ok_or(CodecError::IntegerOverflow)?;}if sum!=0&&sum!=RANS_TOTAL{return Err(CodecError::InvalidEntropyStream)}}let len=usize::try_from(read_u64(data,&mut p)?).map_err(|_|CodecError::IntegerOverflow)?;let end=p.checked_add(len).ok_or(CodecError::IntegerOverflow)?;let payload=data.get(p..end).ok_or(CodecError::Truncated)?;p=end;rans_context_decode(payload,raw_len,&tables)?}
        else if mode==3{let len=usize::try_from(read_u64(data,&mut p)?).map_err(|_|CodecError::IntegerOverflow)?;let end=p.checked_add(len).ok_or(CodecError::IntegerOverflow)?;let payload=data.get(p..end).ok_or(CodecError::Truncated)?;p=end;adaptive_range_decode(payload,raw_len)?}
        else{return Err(CodecError::InvalidEntropyStream)};
        if p!=data.len(){return Err(CodecError::InvalidHeader)}self.decode_stream(&stream,&dict)
    }
    fn decompress_v1(&self,data:&[u8],mut p:usize)->Result<String,CodecError>{
        let n=read_u32(data,&mut p)? as usize;if n>self.limits.max_dictionary_entries{return Err(CodecError::DictionaryTooLarge)}let mut dict=Vec::with_capacity(n);
        for _ in 0..n{let len=read_u32(data,&mut p)? as usize;if len==0||len>self.limits.max_dictionary_entry_bytes{return Err(CodecError::InvalidDictionary)}let end=p.checked_add(len).ok_or(CodecError::IntegerOverflow)?;let b=data.get(p..end).ok_or(CodecError::Truncated)?;p=end;dict.push(std::str::from_utf8(b).map_err(|_|CodecError::InvalidUtf8)?.to_owned());}
        let len=usize::try_from(read_u64(data,&mut p)?).map_err(|_|CodecError::IntegerOverflow)?;if len>self.limits.max_stream_bytes{return Err(CodecError::ArchiveTooLarge)}let end=p.checked_add(len).ok_or(CodecError::IntegerOverflow)?;let stream=data.get(p..end).ok_or(CodecError::Truncated)?;if end!=data.len(){return Err(CodecError::InvalidHeader)}self.decode_stream(stream,&dict)
    }
    fn decode_stream(&self,stream:&[u8],dict:&[String])->Result<String,CodecError>{
        let mut out=String::new();let mut q=0usize;
        while q<stream.len(){if stream[q]==ESCAPE{q+=1;let len=read_u32(stream,&mut q)? as usize;if len==0{return Err(CodecError::InvalidToken)}let end=q.checked_add(len).ok_or(CodecError::IntegerOverflow)?;let raw=stream.get(q..end).ok_or(CodecError::Truncated)?;q=end;let s=std::str::from_utf8(raw).map_err(|_|CodecError::InvalidUtf8)?;let new_len=out.len().checked_add(s.len()).ok_or(CodecError::IntegerOverflow)?;if new_len>self.limits.max_output_bytes{return Err(CodecError::OutputTooLarge)}out.push_str(s);}else{let id=usize::try_from(token_id::decode(stream,&mut q)?).map_err(|_|CodecError::InvalidToken)?;let s=dict.get(id).ok_or(CodecError::InvalidToken)?;let new_len=out.len().checked_add(s.len()).ok_or(CodecError::IntegerOverflow)?;if new_len>self.limits.max_output_bytes{return Err(CodecError::OutputTooLarge)}out.push_str(s);}}
        Ok(out)
    }
    fn candidate_pool(&self,text:&str)->Vec<Candidate>{
        let mut freq:HashMap<String,usize>=HashMap::new();
        // Complete lexical and separator runs are the high-confidence base set.
        add_run_candidates(text,&mut freq,self.min_piece_chars,self.max_piece_chars.saturating_mul(3));
        // Stage 7: morphology and phrase proposals are generated by a separate,
        // Unicode-scalar-safe module. JTCO still decides their actual value.
        stage7::morphology_candidates(text, &mut freq, self.min_piece_chars, self.max_piece_chars);
        stage7::phrase_candidates(text, &mut freq, self.max_piece_chars.saturating_mul(3));
        // Morphology proposals: prefixes, suffixes and internal character n-grams.
        for run in text.split_whitespace(){
            if run.len()>1024{continue}
            let chars:Vec<char>=run.chars().collect();
            // Long unspaced passages are handled by script-aware windows below;
            // avoid quadratic candidate materialization for entire CJK documents.
            if chars.len()<self.min_piece_chars||chars.len()>256{continue}
            let max=self.max_piece_chars.min(chars.len());
            for n in self.min_piece_chars..=max{
                for start in 0..=chars.len()-n{
                    let piece:String=chars[start..start+n].iter().collect();
                    record_candidate(&mut freq,piece);
                }
            }
            for n in self.min_piece_chars..=max.min(8){
                let pre:String=chars[..n].iter().collect();
                let suf:String=chars[chars.len()-n..].iter().collect();
                record_candidate(&mut freq,pre);
                record_candidate(&mut freq,suf);
            }
        }
        // Explicit no-space script segmentation: CJK/Hangul and Southeast Asian
        // scripts receive sliding n-gram candidates even when a document contains
        // no whitespace boundaries. Code-point boundaries remain UTF-8 safe.
        add_script_candidates(text,&mut freq);
        // Repeated two/three-word phrases, streamed with a two-word look-behind
        // so very large documents do not require a second document-sized vector.
        let mut previous:Option<&str>=None;
        let mut previous2:Option<&str>=None;
        let max_phrase_bytes=self.max_piece_chars.saturating_mul(3);
        for word in text.split_whitespace(){
            if let Some(one)=previous{
                if one.len().saturating_add(1).saturating_add(word.len())<=max_phrase_bytes{
                    record_candidate(&mut freq,format!("{one} {word}"));
                }
            }
            if let (Some(two),Some(one))=(previous2,previous){
                if two.len().saturating_add(one.len()).saturating_add(2).saturating_add(word.len())<=max_phrase_bytes{
                    record_candidate(&mut freq,format!("{two} {one} {word}"));
                }
            }
            previous2=previous;previous=Some(word);
        }
        let mut candidates:Vec<Candidate>=freq.into_iter().filter_map(|(piece,count)|{
            if piece.is_empty()||piece.len()>self.limits.max_dictionary_entry_bytes{return None}
            // A cheap ordering hint only. Acceptance is decided by exact full cost below.
            let token_len=token_id::encoded_len(0).unwrap_or(1) as i64;
            let rough=(piece.len() as i64+5-token_len)*(count as i64)-(piece.len() as i64+4);
            Some(Candidate{text:piece,score:rough})
        }).collect();
        let order=|a:&Candidate,b:&Candidate|b.score.cmp(&a.score)
            .then_with(||b.text.len().cmp(&a.text.len()))
            .then_with(||a.text.cmp(&b.text));
        candidates.sort_by(order);
        // Keep a small tail of low/negative-hint candidates for pair look-ahead:
        // two entries can jointly improve tokenization even if neither looks good alone.
        let size_budget=(text.len()/256).clamp(4,128);
        let work_budget=(64usize.saturating_mul(1024*1024)/text.len().max(1)).clamp(4,128);
        let budget=size_budget.min(work_budget).min(self.max_dict_entries);
        let mut positive:Vec<Candidate>=candidates.iter().filter(|c|c.score>0).cloned().collect();
        let mut nonpositive:Vec<Candidate>=candidates.into_iter().filter(|c|c.score<=0).collect();
        nonpositive.sort_by(order);
        let reserve=nonpositive.len().min(8).min(budget/4);
        positive.truncate(budget.saturating_sub(reserve));
        nonpositive.truncate(budget.saturating_sub(positive.len()));
        positive.extend(nonpositive);
        positive.sort_by(order);
        positive
    }
    fn optimize_dictionary(&self,text:&str)->Result<Vec<String>,CodecError>{
        let candidates=self.candidate_pool(text);
        let local_width=(16usize.saturating_mul(1024*1024)/text.len().max(1)).clamp(1,4);
        let local_rounds:usize=if local_width>=3{2}else{1};
        let mut selected:Vec<String>=Vec::new();
        let mut current_cost=self.complete_archive_cost(text,&selected)?;

        // Phase A: greedy additions. Every accepted move is evaluated by a full
        // re-tokenization and exact serialized-size calculation.
        for candidate in &candidates{
            if selected.len()>=self.max_dict_entries{break}
            if selected.iter().any(|x|x==&candidate.text){continue}
            let mut trial=selected.clone();
            trial.push(candidate.text.clone());
            trial.sort_by(|a,b|b.len().cmp(&a.len()).then_with(||a.cmp(b)));
            let trial_cost=self.complete_archive_cost(text,&trial)?;
            if trial_cost<current_cost{selected=trial;current_cost=trial_cost;}
        }

        // Phase B: bounded two-candidate look-ahead. This can discover useful
        // combinations where neither dictionary item pays for itself alone.
        let pair_candidates=low_hint_candidates(&candidates,&selected,local_width);
        for _ in 0..local_rounds{
            if selected.len().saturating_add(2)>self.max_dict_entries{break}
            let mut best:Option<(usize,Vec<String>)>=None;
            for i in 0..pair_candidates.len(){
                for j in i+1..pair_candidates.len(){
                    let mut trial=selected.clone();
                    trial.push(pair_candidates[i].to_owned());
                    trial.push(pair_candidates[j].to_owned());
                    trial.sort_by(|a,b|b.len().cmp(&a.len()).then_with(||a.cmp(b)));
                    let cost=self.complete_archive_cost(text,&trial)?;
                    if cost<best.as_ref().map_or(current_cost,|x|x.0){best=Some((cost,trial));}
                }
            }
            if let Some((cost,dict))=best{if cost<current_cost{selected=dict;current_cost=cost;}else{break}}else{break}
        }

        // Phase C: replacement moves. A candidate may be valuable only after a
        // longer overlapping entry is removed. Keep the search deliberately bounded.
        for _ in 0..local_rounds{
            let incoming=low_hint_candidates(&candidates,&selected,local_width);
            let outgoing=selected.len().min(local_width);
            let mut best:Option<(usize,Vec<String>)>=None;
            for candidate in &incoming{
                for remove_index in 0..outgoing{
                    let mut trial=selected.clone();
                    trial.remove(remove_index);
                    trial.push((*candidate).to_owned());
                    trial.sort_by(|a,b|b.len().cmp(&a.len()).then_with(||a.cmp(b)));
                    let cost=self.complete_archive_cost(text,&trial)?;
                    if cost<best.as_ref().map_or(current_cost,|x|x.0){best=Some((cost,trial));}
                }
            }
            if let Some((cost,dict))=best{if cost<current_cost{selected=dict;current_cost=cost;}else{break}}else{break}
        }
        // Phase D: exact pruning. Greedy construction can leave an entry whose
        // marginal value disappeared after overlapping candidates were accepted.
        let mut prune_rounds=0usize;
        while !selected.is_empty() && prune_rounds<local_rounds.saturating_add(1){
            prune_rounds+=1;
            let mut best:Option<(usize,usize)>=None;
            for idx in 0..selected.len(){
                let mut trial=selected.clone();
                trial.remove(idx);
                let cost=self.complete_archive_cost(text,&trial)?;
                if cost<best.as_ref().map_or(current_cost,|x|x.0){best=Some((cost,idx));}
            }
            if let Some((cost,idx))=best{if cost<current_cost{selected.remove(idx);current_cost=cost;}else{break}}else{break}
        }

        // Phase E: bounded one-for-two replacement search. This is a small
        // neighborhood around the current solution; it complements Phase C and
        // avoids an unbounded combinatorial JTCO search.
        let replacement_pool=low_hint_candidates(&candidates,&selected,local_width.saturating_add(2));
        if selected.len()>=1 && self.max_dict_entries>=selected.len(){
            let remove_count=selected.len().min(local_width);
            let mut best:Option<(usize,Vec<String>)>=None;
            'outer: for incoming in &replacement_pool{
                for remove_idx in 0..remove_count{
                    let mut trial=selected.clone();trial.remove(remove_idx);trial.push((*incoming).to_owned());
                    trial.sort_by(|a,b|b.len().cmp(&a.len()).then_with(||a.cmp(b)));
                    if trial.len()>self.max_dict_entries{continue}
                    let cost=self.complete_archive_cost(text,&trial)?;
                    if cost<best.as_ref().map_or(current_cost,|x|x.0){best=Some((cost,trial));}
                    if replacement_pool.len().saturating_mul(remove_count)>256{break 'outer}
                }
            }
            if let Some((cost,dict))=best{if cost<current_cost{selected=dict;current_cost=cost;}}
        }
        // Phase F: bounded exact neighborhood refinement. Earlier phases use
        // narrow candidate windows to keep construction cheap. Here we revisit a
        // broader deterministic pool and evaluate complete-archive cost for
        // both additions and one-for-one swaps. Every trial re-tokenizes and
        // re-selects the entropy coder, so a move is accepted only when the
        // serialized archive really becomes smaller.
        let refinement_rounds=2usize;
        let mut evaluations=0usize;
        for _ in 0..refinement_rounds {
            let refinement_pool:Vec<&str>=candidates.iter()
                .filter(|c|!selected.iter().any(|x|x==&c.text))
                .take(48)
                .map(|c|c.text.as_str()).collect();
            let mut best:Option<(usize,Vec<String>)>=None;
            // Pure additions are useful when an earlier replacement removed
            // an item that no longer conflicts with the current parse.
            if selected.len()<self.max_dict_entries {
                for candidate in &refinement_pool {
                    if evaluations>=512 { break; }
                    evaluations+=1;
                    let mut trial=selected.clone();
                    trial.push((*candidate).to_owned());
                    trial.sort_by(|a,b|b.len().cmp(&a.len()).then_with(||a.cmp(b)));
                    let cost=self.complete_archive_cost(text,&trial)?;
                    if cost<best.as_ref().map_or(current_cost,|x|x.0) {
                        best=Some((cost,trial));
                    }
                }
            }
            // Full selected-set swaps, not only the first few dictionary IDs.
            'refine: for candidate in &refinement_pool {
                for remove_idx in 0..selected.len() {
                    if evaluations>=512 { break 'refine; }
                    evaluations+=1;
                    let mut trial=selected.clone();
                    trial.remove(remove_idx);
                    trial.push((*candidate).to_owned());
                    trial.sort_by(|a,b|b.len().cmp(&a.len()).then_with(||a.cmp(b)));
                    let cost=self.complete_archive_cost(text,&trial)?;
                    if cost<best.as_ref().map_or(current_cost,|x|x.0) {
                        best=Some((cost,trial));
                    }
                }
            }
            if let Some((cost,dict))=best {
                if cost<current_cost { selected=dict; current_cost=cost; }
                else { break; }
            } else { break; }
        }

        // Phase G: bounded beam search over interacting additions. Unlike the
        // pair look-ahead above, this carries several partial dictionaries forward
        // and explores up to three additions. A candidate that is weak alone can
        // become useful after another token changes the segmentation landscape.
        // Every node is scored with the complete archive cost (dictionary framing,
        // token IDs, re-tokenization and entropy model included).
        let beam_pool:Vec<String>=low_hint_candidates(&candidates,&selected,16)
            .into_iter().map(str::to_owned).collect();
        let beam_width=4usize;
        let beam_depth=3usize;
        let mut beam:Vec<(usize,Vec<String>)>=vec![(current_cost,selected.clone())];
        let mut beam_best:Option<(usize,Vec<String>)>=None;
        let mut beam_evaluations=0usize;
        for _depth in 0..beam_depth {
            let mut next:Vec<(usize,Vec<String>)>=Vec::new();
            for (_,base) in &beam {
                for candidate in &beam_pool {
                    if beam_evaluations>=384 { break; }
                    if base.iter().any(|entry|entry==candidate) || base.len()>=self.max_dict_entries { continue; }
                    let mut trial=base.clone();
                    trial.push(candidate.clone());
                    trial.sort_by(|a,b|b.len().cmp(&a.len()).then_with(||a.cmp(b)));
                    beam_evaluations+=1;
                    let cost=self.complete_archive_cost(text,&trial)?;
                    if cost<current_cost && beam_best.as_ref().map_or(true,|best|cost<best.0) {
                        beam_best=Some((cost,trial.clone()));
                    }
                    if !next.iter().any(|(_,existing)|existing==&trial) {
                        next.push((cost,trial));
                    }
                }
                if beam_evaluations>=384 { break; }
            }
            next.sort_by(|a,b|a.0.cmp(&b.0).then_with(||a.1.cmp(&b.1)));
            next.truncate(beam_width);
            if next.is_empty() { break; }
            beam=next;
        }
        if let Some((cost,dict))=beam_best {
            if cost<current_cost { selected=dict; current_cost=cost; }
        }

        // Ensure output order and duplicate-free dictionary invariants.
        selected.sort_by(|a,b|b.len().cmp(&a.len()).then_with(||a.cmp(b)));
        selected.dedup();
        let selected=self.optimize_token_order(text,selected)?;
        let _final_cost=self.complete_archive_cost(text,&selected)?;
        Ok(selected)
    }
    /// Reassign compact prefix-tier IDs to the tokens actually emitted most often.
    /// The pass is deterministic and bounded; it re-parses after each reassignment
    /// because changing IDs can change the dynamic-programming parse.
    fn optimize_token_order(&self,text:&str,mut dict:Vec<String>)->Result<Vec<String>,CodecError>{
        if dict.len()<2{return Ok(dict)}
        for _ in 0..3 {
            let ids:HashMap<&str,u64>=dict.iter().enumerate().map(|(i,s)|(s.as_str(),i as u64)).collect();
            let stream=self.best_parse_stream(text,&ids)?;
            let mut counts=vec![0u64;dict.len()];
            let mut pos=0usize;
            while pos<stream.len(){
                if stream[pos]==ESCAPE {
                    pos=pos.checked_add(1).ok_or(CodecError::IntegerOverflow)?;
                    let len=read_u32(&stream,&mut pos)? as usize;
                    pos=pos.checked_add(len).ok_or(CodecError::IntegerOverflow)?;
                    if pos>stream.len(){return Err(CodecError::Truncated)}
                } else {
                    let id=usize::try_from(token_id::decode(&stream,&mut pos)?).map_err(|_|CodecError::InvalidToken)?;
                    let count=counts.get_mut(id).ok_or(CodecError::InvalidToken)?;
                    *count=count.saturating_add(1);
                }
            }
            let previous=dict.clone();
            let mut ranked:Vec<(String,u64,usize)>=dict.clone().into_iter().enumerate().map(|(i,s)|(s,counts[i],i)).collect();
            ranked.sort_by(|a,b|b.1.cmp(&a.1)
                .then_with(||token_id::encoded_len(a.2 as u64).cmp(&token_id::encoded_len(b.2 as u64)))
                .then_with(||b.0.len().cmp(&a.0.len()))
                .then_with(||a.0.cmp(&b.0)));
            let next:Vec<String>=ranked.into_iter().map(|x|x.0).collect();
            if next==previous {break}
            dict=next;
        }
        Ok(dict)
    }
    fn complete_archive_cost(&self,text:&str,dict:&[String])->Result<usize,CodecError>{
        let ordered=self.optimize_token_order(text,dict.to_vec())?;
        let dict=&ordered;
        let ids:HashMap<&str,u64>=dict.iter().enumerate().map(|(i,s)|(s.as_str(),i as u64)).collect();
        let stream=self.best_parse_stream(text,&ids)?;
        if stream.len()>self.limits.max_stream_bytes{return Err(CodecError::ArchiveTooLarge)}
        // Exact v2 size: fixed framing, dictionary records, raw stream length,
        // selected entropy metadata and payload. All integer widths match the writer.
        let mut total=4usize+1+4+8+1+8;
        for entry in dict{
            total=total.checked_add(4).and_then(|v|v.checked_add(entry.len())).ok_or(CodecError::IntegerOverflow)?;
        }
        let mut payload_len=stream.len();
        let mut entropy_overhead=0usize;
        if self.use_rans&&!stream.is_empty(){
            let (encoded,_)=rans_encode(&stream);
            if encoded.len().checked_add(512).map_or(false,|v|v<payload_len){
                payload_len=encoded.len();entropy_overhead=512;
            }
            let (context_encoded,_)=rans_context_encode(&stream);
            if context_encoded.len().checked_add(CONTEXT_TABLE_BYTES).map_or(false,|v|v<payload_len+entropy_overhead){
                payload_len=context_encoded.len();entropy_overhead=CONTEXT_TABLE_BYTES;
            }
            let adaptive_payload=adaptive_range_encode(&stream);
            if adaptive_payload.len()<payload_len+entropy_overhead{
                payload_len=adaptive_payload.len();entropy_overhead=0;
            }
        }
        total.checked_add(payload_len).and_then(|v|v.checked_add(entropy_overhead)).ok_or(CodecError::IntegerOverflow)
    }
    fn best_parse_stream(&self,text:&str,ids:&HashMap<&str,u64>)->Result<Vec<u8>,CodecError>{
        // Stage 9 JPEO: evaluate several token/literal parsing biases against the
        // actual entropy payload plus its model metadata. This is a bounded local
        // search, not a claim of globally optimal parsing.
        let mut best:Option<(usize,Vec<u8>)>=None;
        for (token_weight,literal_weight) in [(1usize,1usize),(2,1),(1,2),(3,2)] {
            let mut candidate=Vec::with_capacity(text.len());
            self.encode_text_weighted(text,ids,&mut candidate,token_weight,literal_weight)?;
            let cost=self.entropy_payload_cost(&candidate);
            if best.as_ref().map_or(true,|(current,_)|cost<*current){best=Some((cost,candidate));}
        }
        // Stage 13: context-conditioned parses. The previous Unicode script
        // class acts as a compact, deterministic context state. A token that
        // continues the active script is preferred; a script switch pays a
        // small configurable transition penalty. Final selection still uses
        // the exact entropy payload cost, not this heuristic score.
        for strength in [1usize,2,3] {
            let mut candidate=Vec::with_capacity(text.len());
            self.encode_text_contextual(text,ids,&mut candidate,strength)?;
            let cost=self.entropy_payload_cost(&candidate);
            if best.as_ref().map_or(true,|(current,_)|cost<*current){best=Some((cost,candidate));}
        }
        // Stage 14: train a compact order-2 Markov model over six Unicode
        // writing classes. It is encoder-side parse guidance only: the chosen
        // token stream is self-describing, so no model needs to be stored.
        for strength in [1usize,2,3] {
            let mut candidate=Vec::with_capacity(text.len());
            self.encode_text_predictive(text,ids,&mut candidate,strength)?;
            let cost=self.entropy_payload_cost(&candidate);
            if best.as_ref().map_or(true,|(current,_)|cost<*current){best=Some((cost,candidate));}
        }
        Ok(best.map(|(_,stream)|stream).unwrap_or_default())
    }
    fn encode_text_predictive(&self,text:&str,ids:&HashMap<&str,u64>,out:&mut Vec<u8>,strength:usize)->Result<(),CodecError>{
        if text.is_empty(){return Ok(())}
        if ids.is_empty(){return write_literal(text.as_bytes(),out)}
        const CLASSES:usize=7;
        const STATES:usize=CLASSES*CLASSES*2;
        const UNKNOWN:usize=6;
        #[derive(Clone,Copy)] struct Prev{pos:usize,state:usize,token:Option<u64>}
        let model=ScriptMarkovModel::train(text,ids,self.max_piece_chars);
        let mut boundaries:Vec<usize>=text.char_indices().map(|(i,_)|i).collect();
        boundaries.push(text.len());
        let n=boundaries.len();let inf=usize::MAX/4;
        let mut cost=vec![vec![inf;STATES];n];
        let mut prev:Vec<Vec<Option<Prev>>>=vec![vec![None;STATES];n];
        let start_state=markov_state(UNKNOWN,UNKNOWN,false);
        cost[0][start_state]=0;
        for i in 0..n-1{
            let start=boundaries[i];let next=boundaries[i+1];
            let ch=&text[start..next];
            let literal_class=script_class(ch.chars().next().unwrap_or(' ')) as usize;
            let scan_end=(i+self.max_piece_chars.saturating_mul(3)).min(n-1);
            for state in 0..STATES{
                let base=cost[i][state];if base>=inf{continue}
                let (previous2,previous1,in_literal)=markov_state_parts(state);
                let literal_penalty=model.penalty(previous2,previous1,literal_class).saturating_mul(strength);
                let literal_add=ch.len().saturating_add(if in_literal{0}else{5}).saturating_add(literal_penalty);
                let literal_state=markov_state(previous1,literal_class,true);
                let literal_cost=base.saturating_add(literal_add);
                if literal_cost<cost[i+1][literal_state]{
                    cost[i+1][literal_state]=literal_cost;
                    prev[i+1][literal_state]=Some(Prev{pos:i,state,token:None});
                }
                for j in i+1..=scan_end{
                    let piece=&text[start..boundaries[j]];
                    if let Some(&id)=ids.get(piece){
                        let class=script_class(piece.chars().next().unwrap_or(' ')) as usize;
                        let transition=model.penalty(previous2,previous1,class).saturating_mul(strength);
                        let width=token_id::encoded_len(id).ok_or(CodecError::InvalidToken)?;
                        let candidate=base.saturating_add(width).saturating_add(transition);
                        let target=markov_state(previous1,class,false);
                        if candidate<cost[j][target]{
                            cost[j][target]=candidate;
                            prev[j][target]=Some(Prev{pos:i,state,token:Some(id)});
                        }
                    }
                }
            }
        }
        let mut state=(0..STATES).min_by_key(|&s|cost[n-1][s]).ok_or(CodecError::InvalidToken)?;
        let mut at=n-1;let mut actions:Vec<(usize,usize,Option<u64>)>=Vec::new();
        while at>0{
            let step=prev[at][state].ok_or(CodecError::InvalidToken)?;
            actions.push((boundaries[step.pos],boundaries[at],step.token));
            at=step.pos;state=step.state;
        }
        actions.reverse();let mut literal_start:Option<usize>=None;let mut literal_end=0usize;
        for(start,end,token)in actions{
            if let Some(id)=token{
                if let Some(ls)=literal_start.take(){write_literal(&text.as_bytes()[ls..literal_end],out)?;}
                token_id::encode(id,out)?;
            }else{
                if literal_start.is_none(){literal_start=Some(start);}literal_end=end;
            }
        }
        if let Some(ls)=literal_start{write_literal(&text.as_bytes()[ls..literal_end],out)?;}
        Ok(())
    }
    fn encode_text_contextual(&self,text:&str,ids:&HashMap<&str,u64>,out:&mut Vec<u8>,switch_penalty:usize)->Result<(),CodecError>{
        if text.is_empty(){return Ok(())}
        if ids.is_empty(){return write_literal(text.as_bytes(),out)}
        #[derive(Clone,Copy)] struct Prev{pos:usize,state:usize,token:Option<u64>}
        let mut boundaries:Vec<usize>=text.char_indices().map(|(i,_)|i).collect();
        boundaries.push(text.len());
        let n=boundaries.len();let inf=usize::MAX/4;
        let mut cost=vec![[inf;2];n];let mut prev:Vec<[Option<Prev>;2]>=vec![[None,None];n];
        cost[0][0]=0;
        for i in 0..n-1{
            let start=boundaries[i];let next=boundaries[i+1];let ch_bytes=next-start;
            for state in 0..2{if cost[i][state]<inf{
                let add=ch_bytes.saturating_add(if state==0{5}else{0});
                let candidate=cost[i][state].saturating_add(add);
                if candidate<cost[i+1][1]{cost[i+1][1]=candidate;prev[i+1][1]=Some(Prev{pos:i,state,token:None});}
            }}
            let scan_end=(i+self.max_piece_chars.saturating_mul(3)).min(n-1);
            let previous_class=if start==0{None}else{text[..start].chars().next_back().and_then(script_group)};
            for j in i+1..=scan_end{
                let piece=&text[start..boundaries[j]];
                if let Some(&id)=ids.get(piece){
                    let token_class=piece.chars().next().and_then(script_group);
                    let transition=match(previous_class,token_class){(Some(a),Some(b)) if a!=b=>switch_penalty,_=>0};
                    let bytes=token_id::encoded_len(id).ok_or(CodecError::InvalidToken)?;
                    for state in 0..2{if cost[i][state]<inf{
                        let candidate=cost[i][state].saturating_add(bytes).saturating_add(transition);
                        if candidate<cost[j][0]{cost[j][0]=candidate;prev[j][0]=Some(Prev{pos:i,state,token:Some(id)});}
                    }}
                }
            }
        }
        let mut state=if cost[n-1][0]<=cost[n-1][1]{0}else{1};let mut at=n-1;
        let mut actions:Vec<(usize,usize,Option<u64>)>=Vec::new();
        while at>0{let step=prev[at][state].ok_or(CodecError::InvalidToken)?;actions.push((boundaries[step.pos],boundaries[at],step.token));at=step.pos;state=step.state;}
        actions.reverse();let mut literal_start:Option<usize>=None;let mut literal_end=0usize;
        for(start,end,token)in actions{if let Some(id)=token{if let Some(ls)=literal_start.take(){write_literal(&text.as_bytes()[ls..literal_end],out)?;}token_id::encode(id,out)?;}else{if literal_start.is_none(){literal_start=Some(start);}literal_end=end;}}
        if let Some(ls)=literal_start{write_literal(&text.as_bytes()[ls..literal_end],out)?;}
        Ok(())
    }
    fn entropy_payload_cost(&self,stream:&[u8])->usize{
        let mut best=stream.len();
        if self.use_rans&&!stream.is_empty(){
            let (encoded,_)=rans_encode(stream); best=best.min(encoded.len().saturating_add(512));
            let (context,_)=rans_context_encode(stream); best=best.min(context.len().saturating_add(CONTEXT_TABLE_BYTES));
            best=best.min(adaptive_range_encode(stream).len());
        }
        best
    }
    fn encode_text(&self,text:&str,ids:&HashMap<&str,u64>,out:&mut Vec<u8>)->Result<(),CodecError>{
        self.encode_text_weighted(text,ids,out,1,1)
    }
    fn encode_text_weighted(&self,text:&str,ids:&HashMap<&str,u64>,out:&mut Vec<u8>,token_weight:usize,literal_weight:usize)->Result<(),CodecError>{
        // Stage 8 PTC + Stage 9 weighted candidates: DP over UTF-8 boundaries.
        // actual token-ID byte lengths against batched literal-run cost (5-byte
        // escape header per run + literal bytes), rather than greedily taking the
        // longest dictionary match. Ties prefer fewer tokens and deterministic order.
        if text.is_empty(){return Ok(())}
        if ids.is_empty(){return write_literal(text.as_bytes(),out)}
        #[derive(Clone,Copy)] struct Prev{pos:usize,state:usize,token:Option<u64>}
        let mut boundaries:Vec<usize>=text.char_indices().map(|(i,_)|i).collect();
        boundaries.push(text.len());
        let n=boundaries.len();
        let inf=usize::MAX/4;
        let mut cost=vec![[inf;2];n];
        let mut prev:Vec<[Option<Prev>;2]>=vec![[None,None];n];
        cost[0][0]=0;
        for i in 0..n-1{
            let start=boundaries[i];let next=boundaries[i+1];
            let ch_bytes=next-start;
            // Literal transition: opening a run pays ESCAPE + u32 length once.
            for state in 0..2{
                if cost[i][state]<inf{
                    let add=ch_bytes.saturating_add(if state==0{5}else{0}).saturating_mul(literal_weight);
                    let candidate=cost[i][state].saturating_add(add);
                    if candidate<cost[i+1][1]{cost[i+1][1]=candidate;prev[i+1][1]=Some(Prev{pos:i,state,token:None});}
                }
            }
            // Every dictionary match is a candidate edge. Limit scan by the same
            // bounded piece window used during candidate generation.
            let scan_end=(i+self.max_piece_chars.saturating_mul(3)).min(n-1);
            for j in i+1..=scan_end{
                let piece=&text[start..boundaries[j]];
                if let Some(&id)=ids.get(piece){
                    let bytes=token_id::encoded_len(id).ok_or(CodecError::InvalidToken)?;
                    for state in 0..2{
                        if cost[i][state]<inf{
                            let candidate=cost[i][state].saturating_add(bytes.saturating_mul(token_weight));
                            if candidate<cost[j][0]{cost[j][0]=candidate;prev[j][0]=Some(Prev{pos:i,state,token:Some(id)});}
                        }
                    }
                }
            }
        }
        let mut state=if cost[n-1][0]<=cost[n-1][1]{0}else{1};
        let mut at=n-1;let mut actions:Vec<(usize,usize,Option<u64>)>=Vec::new();
        while at>0{
            let step=prev[at][state].ok_or(CodecError::InvalidToken)?;
            actions.push((boundaries[step.pos],boundaries[at],step.token));
            at=step.pos;state=step.state;
        }
        actions.reverse();
        let mut literal_start:Option<usize>=None;let mut literal_end=0usize;
        for (start,end,token) in actions{
            if let Some(id)=token{
                if let Some(ls)=literal_start.take(){write_literal(&text.as_bytes()[ls..literal_end],out)?;}
                token_id::encode(id,out)?;
            }else{
                if literal_start.is_none(){literal_start=Some(start);}
                literal_end=end;
            }
        }
        if let Some(ls)=literal_start{write_literal(&text.as_bytes()[ls..literal_end],out)?;}
        Ok(())
    }
}
fn low_hint_candidates<'a>(candidates:&'a[Candidate],selected:&[String],limit:usize)->Vec<&'a str>{
    let ordered=candidates.iter().filter(|c|c.score<=0)
        .chain(candidates.iter().rev().filter(|c|c.score>0));
    let mut result:Vec<&'a str>=Vec::new();
    if limit==0{return result}
    for candidate in ordered{
        let text=candidate.text.as_str();
        if selected.iter().any(|entry|entry.as_str()==text)||result.iter().any(|&entry|entry==text){continue}
        result.push(text);
        if result.len()>=limit{break}
    }
    result
}

fn write_literal(bytes:&[u8],out:&mut Vec<u8>)->Result<(),CodecError>{if bytes.is_empty(){return Ok(())}out.push(ESCAPE);put_u32(out,u32::try_from(bytes.len()).map_err(|_|CodecError::ArchiveTooLarge)?);out.extend_from_slice(bytes);Ok(())}
// Seven compact writing classes used by the encoder-side order-2 Markov model:
// Latin, Cyrillic, CJK/Hangul, Southeast Asian, Indic, RTL, and other/unknown.
fn script_class(c:char)->u8{
    let u=c as u32;
    if c.is_ascii_alphabetic(){return 0}
    if (0x0400..=0x052F).contains(&u)||(0x2DE0..=0x2DFF).contains(&u)||(0xA640..=0xA69F).contains(&u){return 1}
    script_group(c).map(|v|v+1).unwrap_or(6)
}
fn markov_state(previous2:usize,previous1:usize,in_literal:bool)->usize{
    (previous2*7+previous1)*2+if in_literal{1}else{0}
}
fn markov_state_parts(state:usize)->(usize,usize,bool){
    let in_literal=state%2!=0;let pair=state/2;(pair/7,pair%7,in_literal)
}

#[derive(Clone)]
struct ScriptMarkovModel{
    unigram:[u32;7],
    bigram:[[u32;7];7],
    trigram:[[[u32;7];7];7],
}
impl ScriptMarkovModel{
    fn train(text:&str,ids:&HashMap<&str,u64>,max_piece_chars:usize)->Self{
        // Train on a deterministic greedy token sequence. Unmatched Unicode
        // scalars are represented as literal events in the same class model.
        let mut boundaries:Vec<usize>=text.char_indices().map(|(i,_)|i).collect();
        boundaries.push(text.len());
        let mut classes=Vec::new();let mut i=0usize;
        while i+1<boundaries.len(){
            let end=(i+max_piece_chars.saturating_mul(3)).min(boundaries.len()-1);
            let mut matched=None;
            for j in i+1..=end{if ids.contains_key(&text[boundaries[i]..boundaries[j]]){matched=Some(j);}}
            let ch=text[boundaries[i]..boundaries[i+1]].chars().next().unwrap_or(' ');
            classes.push(script_class(ch) as usize);
            i=matched.unwrap_or(i+1);
        }
        Self::from_classes(&classes)
    }
    fn from_classes(classes:&[usize])->Self{
        let mut model=Self{unigram:[0;7],bigram:[[0;7];7],trigram:[[[0;7];7];7]};
        let mut p2=5usize;let mut p1=5usize;
        for &next in classes{
            if next>=7{continue}
            model.unigram[next]=model.unigram[next].saturating_add(1);
            model.bigram[p1][next]=model.bigram[p1][next].saturating_add(1);
            model.trigram[p2][p1][next]=model.trigram[p2][p1][next].saturating_add(1);
            p2=p1;p1=next;
        }
        model
    }
    fn penalty(&self,p2:usize,p1:usize,next:usize)->usize{
        let context_total:u64=self.trigram[p2][p1].iter().map(|&v|v as u64).sum();
        let observed=self.trigram[p2][p1][next] as u64;
        let (count,total)=if context_total>0{(observed,context_total)}else{
            let total:u64=self.bigram[p1].iter().map(|&v|v as u64).sum();
            (self.bigram[p1][next] as u64,total)
        };
        let (count,total)=if total>0{(count,total)}else{
            (self.unigram[next] as u64,self.unigram.iter().map(|&v|v as u64).sum())
        };
        // Add-one smoothing over seven classes; convert the conditional
        // probability to a small integer surprisal proxy in [0,4].
        let scaled=((count+1).saturating_mul(32)/(total+7).max(1)).clamp(1,32) as u32;
        let mut log2=0usize;let mut value=scaled;
        while value>=2{value>>=1;log2+=1;}
        5usize.saturating_sub(log2)
    }
}

fn script_group(c:char)->Option<u8>{
    let u=c as u32;
    if (0x3400..=0x4DBF).contains(&u)||(0x4E00..=0x9FFF).contains(&u)
        ||(0xF900..=0xFAFF).contains(&u)||(0x3040..=0x30FF).contains(&u)
        ||(0x31F0..=0x31FF).contains(&u)||(0xAC00..=0xD7AF).contains(&u)
        ||(0x1100..=0x11FF).contains(&u)||(0x3130..=0x318F).contains(&u){return Some(1)}
    // Group 2: scripts commonly written without ASCII whitespace boundaries.
    if (0x0E00..=0x0E7F).contains(&u)||(0x0E80..=0x0EFF).contains(&u)
        ||(0x1780..=0x17FF).contains(&u)||(0x1000..=0x109F).contains(&u){return Some(2)}
    // Group 3: Indic scripts. Generate reusable character sequences while
    // retaining combining marks with the active script run.
    if (0x0900..=0x097F).contains(&u)||(0x0980..=0x09FF).contains(&u)
        ||(0x0A00..=0x0A7F).contains(&u)||(0x0A80..=0x0AFF).contains(&u)
        ||(0x0B00..=0x0B7F).contains(&u)||(0x0B80..=0x0BFF).contains(&u)
        ||(0x0C00..=0x0C7F).contains(&u)||(0x0C80..=0x0CFF).contains(&u)
        ||(0x0D00..=0x0D7F).contains(&u)||(0xA800..=0xA82F).contains(&u){return Some(3)}
    // Group 4: RTL alphabetic runs (Arabic and Hebrew families).
    if (0x0590..=0x05FF).contains(&u)||(0x0600..=0x06FF).contains(&u)
        ||(0x0750..=0x077F).contains(&u)||(0x08A0..=0x08FF).contains(&u){return Some(4)}
    None
}
fn is_combining_mark(c:char)->bool{
    let u=c as u32;
    (0x0300..=0x036F).contains(&u)||(0x1AB0..=0x1AFF).contains(&u)
        ||(0x1DC0..=0x1DFF).contains(&u)||(0x20D0..=0x20FF).contains(&u)
        ||(0xFE20..=0xFE2F).contains(&u)
}
fn record_candidate(freq:&mut HashMap<String,usize>,piece:String){
    if let Some(count)=freq.get_mut(&piece){*count=(*count).saturating_add(1);}
    else if freq.len()<100_000{freq.insert(piece,1);}
}
fn add_script_candidates(text:&str,freq:&mut HashMap<String,usize>){
    // A rolling window keeps memory bounded even for multi-gigabyte CJK passages.
    let mut active:Option<u8>=None;
    let mut window:Vec<char>=Vec::with_capacity(10);
    for c in text.chars(){
        let group=script_group(c).or_else(||if is_combining_mark(c){active}else{None});
        if group!=active{window.clear();active=group;}
        if let Some(group)=group{
            window.push(c);
            if window.len()>10{window.remove(0);}
            let min_n=if group==1{1}else{2};
            let max_n=if group==4{window.len().min(6)}else{window.len()};
            for n in min_n..=max_n{
                let token:String=window[window.len()-n..].iter().collect();
                record_candidate(freq,token);
            }
        }
    }
}
fn add_run_candidates(text:&str,freq:&mut HashMap<String,usize>,min_chars:usize,max_chars:usize){
    let mut start=0usize;
    let mut mode:Option<bool>=None;
    for (index,ch) in text.char_indices(){
        let current=ch.is_alphanumeric()||ch=='_'||ch=='-';
        if mode.map_or(false,|previous|previous!=current){
            let run=&text[start..index];
            if run.len()<=max_chars.saturating_mul(4){
                let count=run.chars().count();
                if count>=min_chars&&count<=max_chars{record_candidate(freq,run.to_owned());}
            }
            start=index;
        }
        mode=Some(current);
    }
    if start<text.len(){
        let run=&text[start..];
        if run.len()<=max_chars.saturating_mul(4){
            let count=run.chars().count();
            if count>=min_chars&&count<=max_chars{record_candidate(freq,run.to_owned());}
        }
    }
}
fn put_u16(o:&mut Vec<u8>,v:u16){o.extend_from_slice(&v.to_be_bytes())}fn put_u32(o:&mut Vec<u8>,v:u32){o.extend_from_slice(&v.to_be_bytes())}fn put_u64(o:&mut Vec<u8>,v:u64){o.extend_from_slice(&v.to_be_bytes())}
fn read_u16(d:&[u8],p:&mut usize)->Result<u16,CodecError>{let e=p.checked_add(2).ok_or(CodecError::IntegerOverflow)?;let b=d.get(*p..e).ok_or(CodecError::Truncated)?;*p=e;Ok(u16::from_be_bytes([b[0],b[1]]))}
fn read_u32(d:&[u8],p:&mut usize)->Result<u32,CodecError>{let e=p.checked_add(4).ok_or(CodecError::IntegerOverflow)?;let b=d.get(*p..e).ok_or(CodecError::Truncated)?;*p=e;Ok(u32::from_be_bytes([b[0],b[1],b[2],b[3]]))}
fn read_u64(d:&[u8],p:&mut usize)->Result<u64,CodecError>{let e=p.checked_add(8).ok_or(CodecError::IntegerOverflow)?;let b=d.get(*p..e).ok_or(CodecError::Truncated)?;*p=e;Ok(u64::from_be_bytes([b[0],b[1],b[2],b[3],b[4],b[5],b[6],b[7]]))}

/// Static byte-frequency rANS. Returns payload and normalized frequencies summing to 4096.
fn rans_encode(input:&[u8])->(Vec<u8>,[u16;256]){
    let mut counts=[0u64;256];for &b in input{counts[b as usize]+=1}
    let mut f=[0u16;256];if input.is_empty(){return (Vec::new(),f)}
    let used=counts.iter().filter(|&&x|x>0).count();let mut rems=Vec::new();let mut sum=0u32;
    for i in 0..256{if counts[i]>0{let scaled=counts[i]*RANS_TOTAL as u64/input.len() as u64;let v=scaled.max(1).min((RANS_TOTAL-(used as u32)+1)as u64) as u16;f[i]=v;sum+=v as u32;let rem=counts[i]*RANS_TOTAL as u64%input.len() as u64;rems.push((rem,i));}}
    while sum<RANS_TOTAL{rems.sort_by(|a,b|b.0.cmp(&a.0).then_with(||a.1.cmp(&b.1)));for &(_,i) in &rems{if sum==RANS_TOTAL{break}f[i]+=1;sum+=1}}
    while sum>RANS_TOTAL{let mut idx=None;for i in 0..256{if f[i]>1&&idx.map_or(true,|j:usize|f[i]>f[j]){idx=Some(i)}}if let Some(i)=idx{f[i]-=1;sum-=1}else{break}}
    let mut cum=[0u32;256];let mut c=0u32;for i in 0..256{cum[i]=c;c+=f[i] as u32}
    let mut state=RANS_L;let mut emitted=Vec::new();for &b in input.iter().rev(){let i=b as usize;let freq=f[i] as u32;if freq==0{continue}let x_max=((RANS_L>>RANS_SCALE)<<8)*freq;while state>=x_max{emitted.push((state&0xff)as u8);state>>=8}state=((state/freq)<<RANS_SCALE)+(state%freq)+cum[i];}
    emitted.reverse();let mut out=state.to_le_bytes().to_vec();out.extend_from_slice(&emitted);(out,f)
}
fn rans_decode(data:&[u8],out_len:usize,f:&[u16;256])->Result<Vec<u8>,CodecError>{
    if out_len==0{return if data.is_empty(){Ok(Vec::new())}else{Err(CodecError::InvalidEntropyStream)}}
    if data.len()<4{return Err(CodecError::Truncated)}let mut state=u32::from_le_bytes([data[0],data[1],data[2],data[3]]);if state<RANS_L{return Err(CodecError::InvalidEntropyStream)}
    let mut cum=[0u32;256];let mut lut=[0u8;RANS_TOTAL as usize];let mut c=0u32;for i in 0..256{cum[i]=c;for _ in 0..f[i]{if c>=RANS_TOTAL{return Err(CodecError::InvalidEntropyStream)}lut[c as usize]=i as u8;c+=1}}if c!=RANS_TOTAL{return Err(CodecError::InvalidEntropyStream)}
    let mut p=4usize;let mut out=Vec::with_capacity(out_len);for _ in 0..out_len{let slot=state&(RANS_TOTAL-1);let sym=lut[slot as usize] as usize;let freq=f[sym] as u32;if freq==0{return Err(CodecError::InvalidEntropyStream)}state=freq*(state>>RANS_SCALE)+(slot-cum[sym]);while state<RANS_L{let b=*data.get(p).ok_or(CodecError::Truncated)?;p+=1;state=(state<<8)|b as u32}out.push(sym as u8)}if p!=data.len(){return Err(CodecError::InvalidEntropyStream)}Ok(out)
}

/// Four deterministic byte classes feed the 21-state two-byte-history context map.
/// Encoder and decoder derive the same context from already-known neighboring bytes.
fn byte_class(byte:u8)->usize{
    if byte.is_ascii_whitespace(){0}
    else if byte.is_ascii_alphanumeric(){1}
    else if !byte.is_ascii(){3}
    else{2}
}
/// Two-byte class history: one start context, four one-byte-prefix contexts,
/// and sixteen ordered pairs of previous-byte classes.
fn byte_context(previous2:Option<u8>,previous1:Option<u8>)->usize{
    match (previous2,previous1){
        (None,None)=>0,
        (None,Some(last))=>1+byte_class(last),
        (Some(before),Some(last))=>5+byte_class(before)*4+byte_class(last),
        (Some(_),None)=>0,
    }
}
/// Adaptive range coder's richer byte classes: whitespace, letters, digits,
/// punctuation, non-ASCII and remaining ASCII. History is reduced to five classes.
fn adaptive_byte_class(byte:u8)->usize{
    if byte.is_ascii_whitespace(){0}
    else if byte.is_ascii_alphabetic(){1}
    else if byte.is_ascii_digit(){2}
    else if byte.is_ascii_punctuation(){3}
    else if !byte.is_ascii(){4}
    else{5}
}
/// 32 adaptive contexts: start (1), first-byte classes (6), and ordered
/// two-byte history classes (5x5). This is separate from the fixed rANS v2
/// context table to preserve the archive format's v2 compatibility.
fn adaptive_context(previous2:Option<u8>,previous1:Option<u8>)->usize{
    fn history_class(b:u8)->usize{
        if b.is_ascii_whitespace(){0}
        else if b.is_ascii_alphanumeric(){1}
        else if b.is_ascii_punctuation(){2}
        else if !b.is_ascii(){3}
        else{4}
    }
    match (previous2,previous1){
        (None,None)=>0,
        (None,Some(last))=>1+adaptive_byte_class(last),
        (Some(before),Some(last))=>7+history_class(before)*5+history_class(last),
        (Some(_),None)=>0,
    }
}
fn normalize_counts(counts:&[u64;256],total:u64)->[u16;256]{
    let mut frequencies=[0u16;256];
    if total==0{return frequencies}
    let mut remainders:Vec<(u64,usize)>=Vec::new();
    let mut sum=0u32;
    for i in 0..256{
        if counts[i]>0{
            let scaled=counts[i]*RANS_TOTAL as u64/total;
            frequencies[i]=scaled.max(1) as u16;
            sum+=frequencies[i] as u32;
            remainders.push((counts[i]*RANS_TOTAL as u64%total,i));
        }
    }
    while sum<RANS_TOTAL{
        remainders.sort_by(|a,b|b.0.cmp(&a.0).then_with(||a.1.cmp(&b.1)));
        for &(_,i) in &remainders{if sum==RANS_TOTAL{break}frequencies[i]+=1;sum+=1;}
    }
    while sum>RANS_TOTAL{
        let mut largest=None;
        for i in 0..256{if frequencies[i]>1&&largest.map_or(true,|j:usize|frequencies[i]>frequencies[j]){largest=Some(i)}}
        if let Some(i)=largest{frequencies[i]-=1;sum-=1}else{break}
    }
    frequencies
}
fn rans_context_encode(input:&[u8])->(Vec<u8>,[[u16;256];CONTEXT_COUNT]){
    if input.is_empty(){return (Vec::new(),[[0u16;256];CONTEXT_COUNT])}
    let mut counts=[[0u64;256];CONTEXT_COUNT];
    let mut totals=[0u64;CONTEXT_COUNT];
    for (i,&symbol) in input.iter().enumerate(){
        let context=byte_context(if i<2{None}else{Some(input[i-2])},if i==0{None}else{Some(input[i-1])});
        counts[context][symbol as usize]+=1;totals[context]+=1;
    }
    let mut tables=[[0u16;256];CONTEXT_COUNT];
    for c in 0..CONTEXT_COUNT{tables[c]=normalize_counts(&counts[c],totals[c]);}
    let mut cumulative=[[0u32;256];CONTEXT_COUNT];
    for c in 0..CONTEXT_COUNT{let mut sum=0u32;for i in 0..256{cumulative[c][i]=sum;sum+=tables[c][i] as u32;}}
    let mut state=RANS_L;
    let mut emitted=Vec::new();
    for i in (0..input.len()).rev(){
        let context=byte_context(if i<2{None}else{Some(input[i-2])},if i==0{None}else{Some(input[i-1])});
        let symbol=input[i] as usize;
        let frequency=tables[context][symbol] as u32;
        if frequency==0{continue}
        let x_max=((RANS_L>>RANS_SCALE)<<8)*frequency;
        while state>=x_max{emitted.push((state&0xff)as u8);state>>=8;}
        state=((state/frequency)<<RANS_SCALE)+(state%frequency)+cumulative[context][symbol];
    }
    emitted.reverse();
    let mut payload=state.to_le_bytes().to_vec();payload.extend_from_slice(&emitted);
    (payload,tables)
}
fn rans_context_decode(data:&[u8],out_len:usize,tables:&[[u16;256];CONTEXT_COUNT])->Result<Vec<u8>,CodecError>{
    if out_len==0{return if data.is_empty(){Ok(Vec::new())}else{Err(CodecError::InvalidEntropyStream)}}
    if data.len()<4{return Err(CodecError::Truncated)}
    let mut cumulative=[[0u32;256];CONTEXT_COUNT];
    let mut lookup=[[0u8;RANS_TOTAL as usize];CONTEXT_COUNT];
    for c in 0..CONTEXT_COUNT{
        let mut sum=0u32;
        for symbol in 0..256{
            cumulative[c][symbol]=sum;
            for _ in 0..tables[c][symbol]{
                if sum>=RANS_TOTAL{return Err(CodecError::InvalidEntropyStream)}
                lookup[c][sum as usize]=symbol as u8;sum+=1;
            }
        }
        if sum!=0&&sum!=RANS_TOTAL{return Err(CodecError::InvalidEntropyStream)}
    }
    let mut state=u32::from_le_bytes([data[0],data[1],data[2],data[3]]);
    if state<RANS_L{return Err(CodecError::InvalidEntropyStream)}
    let mut cursor=4usize;
    let mut output=Vec::with_capacity(out_len);
    for _ in 0..out_len{
        let context=byte_context(output.iter().rev().nth(1).copied(),output.last().copied());
        if tables[context].iter().all(|&f|f==0){return Err(CodecError::InvalidEntropyStream)}
        let slot=state&(RANS_TOTAL-1);
        let symbol=lookup[context][slot as usize] as usize;
        let frequency=tables[context][symbol] as u32;
        if frequency==0{return Err(CodecError::InvalidEntropyStream)}
        state=frequency*(state>>RANS_SCALE)+(slot-cumulative[context][symbol]);
        while state<RANS_L{
            let byte=*data.get(cursor).ok_or(CodecError::Truncated)?;
            cursor+=1;state=(state<<8)|byte as u32;
        }
        output.push(symbol as u8);
    }
    if cursor!=data.len(){return Err(CodecError::InvalidEntropyStream)}
    Ok(output)
}

const RANGE_TOP:u64=(1u64<<32)-1;
const RANGE_HALF:u64=1u64<<31;
const RANGE_QUARTER:u64=1u64<<30;
const RANGE_THREE_QUARTERS:u64=3u64<<30;

struct AdaptiveModel{
    trees:[[u32;257];ADAPTIVE_CONTEXT_COUNT],
    totals:[u32;ADAPTIVE_CONTEXT_COUNT],
}
impl AdaptiveModel{
    fn new()->Self{
        let mut model=Self{trees:[[0u32;257];ADAPTIVE_CONTEXT_COUNT],totals:[0u32;ADAPTIVE_CONTEXT_COUNT]};
        for context in 0..ADAPTIVE_CONTEXT_COUNT{for symbol in 0..256{model.add(context,symbol,1);}}
        model.totals=[256u32;ADAPTIVE_CONTEXT_COUNT];
        model
    }
    fn prefix(&self,context:usize,end:usize)->u32{
        let mut index=end;let mut sum=0u32;
        while index>0{sum+=self.trees[context][index];index&=index-1;}
        sum
    }
    fn add(&mut self,context:usize,symbol:usize,delta:i32){
        let mut index=symbol+1;
        while index<=256{
            if delta>=0{self.trees[context][index]+=delta as u32;}
            else{self.trees[context][index]-=(-delta) as u32;}
            index+=index&(!index+1);
        }
    }
    fn interval(&self,context:usize,symbol:usize)->(u32,u32,u32){
        let low=self.prefix(context,symbol);
        let high=self.prefix(context,symbol+1);
        (low,high,self.totals[context])
    }
    fn find_symbol(&self,context:usize,value:u32)->usize{
        let tree=&self.trees[context];
        let mut index=0usize;let mut sum=0u32;let mut step=128usize;
        while step>0{
            let next=index+step;
            if next<=256&&sum+tree[next]<=value{index=next;sum+=tree[next];}
            step>>=1;
        }
        index.min(255)
    }
    fn update(&mut self,context:usize,symbol:usize){
        self.add(context,symbol,1);self.totals[context]+=1;
        if self.totals[context]>=32_768{
            let mut total=0u32;
            for item in 0..256{
                let old=self.prefix(context,item+1)-self.prefix(context,item);
                let new=((old+1)/2).max(1);
                if new!=old{self.add(context,item,new as i32-old as i32);}
                total+=new;
            }
            self.totals[context]=total;
        }
    }
}
struct BitWriter{bytes:Vec<u8>,current:u8,used:u8}
impl BitWriter{
    fn new()->Self{Self{bytes:Vec::new(),current:0,used:0}}
    fn write(&mut self,bit:u8){
        self.current=(self.current<<1)|(bit&1);self.used+=1;
        if self.used==8{self.bytes.push(self.current);self.current=0;self.used=0;}
    }
    fn finish_with_guard(mut self)->(usize,Vec<u8>){
        // Arithmetic decoders initialize a 32-bit code register. Explicit zero guard
        // bits make short streams deterministic and permit strict truncation checks.
        for _ in 0..32{self.write(0);}
        if self.used>0{self.current<<=8-self.used;self.bytes.push(self.current);}
        (self.bytes.len()*8,self.bytes)
    }
}
struct BitReader<'a>{bytes:&'a[u8],bit_pos:usize,bit_len:usize}
impl<'a> BitReader<'a>{
    fn read(&mut self)->Result<u8,CodecError>{
        if self.bit_pos>=self.bit_len{return Err(CodecError::Truncated)}
        let bit=(self.bytes[self.bit_pos/8]>>(7-(self.bit_pos%8)))&1;
        self.bit_pos+=1;Ok(bit)
    }
    fn remaining_is_zero(&self)->bool{
        (self.bit_pos..self.bit_len).all(|pos|((self.bytes[pos/8]>>(7-(pos%8)))&1)==0)
    }
}
fn emit_bit_follow(writer:&mut BitWriter,bit:u8,pending:&mut usize){
    writer.write(bit);
    for _ in 0..*pending{writer.write(1-bit);}
    *pending=0;
}
fn crc32(data:&[u8])->u32{
    let mut crc=0xFFFF_FFFFu32;
    for &byte in data{
        crc^=byte as u32;
        for _ in 0..8{crc=(crc>>1)^(0xEDB8_8320u32&0u32.wrapping_sub(crc&1));}
    }
    !crc
}
fn adaptive_range_encode(input:&[u8])->Vec<u8>{
    if input.is_empty(){return Vec::new()}
    let mut model=AdaptiveModel::new();let mut writer=BitWriter::new();
    let mut low=0u64;let mut high=RANGE_TOP;let mut pending=0usize;
    for (index,&symbol_byte) in input.iter().enumerate(){
        let context=adaptive_context(if index<2{None}else{Some(input[index-2])},if index==0{None}else{Some(input[index-1])});
        let symbol=symbol_byte as usize;
        let (cum_low,cum_high,total)=model.interval(context,symbol);
        let range=high-low+1;
        high=low+(range*cum_high as u64/total as u64)-1;
        low=low+range*cum_low as u64/total as u64;
        loop{
            if high<RANGE_HALF{emit_bit_follow(&mut writer,0,&mut pending);}
            else if low>=RANGE_HALF{emit_bit_follow(&mut writer,1,&mut pending);low-=RANGE_HALF;high-=RANGE_HALF;}
            else if low>=RANGE_QUARTER&&high<RANGE_THREE_QUARTERS{pending+=1;low-=RANGE_QUARTER;high-=RANGE_QUARTER;}
            else{break}
            low<<=1;high=(high<<1)|1;
        }
        model.update(context,symbol);
    }
    pending+=1;
    emit_bit_follow(&mut writer,if low<RANGE_QUARTER{0}else{1},&mut pending);
    let (bit_len,bitstream)=writer.finish_with_guard();
    let mut payload=Vec::with_capacity(8+bitstream.len()+4);
    put_u64(&mut payload,bit_len as u64);
    payload.extend_from_slice(&bitstream);
    put_u32(&mut payload,crc32(input));
    payload
}
fn adaptive_range_decode(data:&[u8],out_len:usize)->Result<Vec<u8>,CodecError>{
    if out_len==0{return if data.is_empty(){Ok(Vec::new())}else{Err(CodecError::InvalidEntropyStream)}}
    if data.len()<12{return Err(CodecError::Truncated)}
    let mut p=0usize;
    let bit_len=usize::try_from(read_u64(data,&mut p)?).map_err(|_|CodecError::IntegerOverflow)?;
    if bit_len<32||bit_len%8!=0{return Err(CodecError::InvalidEntropyStream)}
    let byte_len=bit_len/8;
    let expected=p.checked_add(byte_len).and_then(|v|v.checked_add(4)).ok_or(CodecError::IntegerOverflow)?;
    if expected!=data.len(){return Err(CodecError::InvalidEntropyStream)}
    let bitstream=&data[p..p+byte_len];p+=byte_len;
    let expected_crc=read_u32(data,&mut p)?;
    let mut reader=BitReader{bytes:bitstream,bit_pos:0,bit_len};
    let mut model=AdaptiveModel::new();
    let mut low=0u64;let mut high=RANGE_TOP;let mut code=0u64;
    for _ in 0..32{code=(code<<1)|reader.read()? as u64;}
    let mut output=Vec::with_capacity(out_len);
    for _ in 0..out_len{
        let context=adaptive_context(output.iter().rev().nth(1).copied(),output.last().copied());
        if low>high||code<low||code>high{return Err(CodecError::InvalidEntropyStream)}
        let total=model.totals[context] as u64;
        let range=high-low+1;
        let scaled=(((code-low+1)*total)-1)/range;
        if scaled>=total{return Err(CodecError::InvalidEntropyStream)}
        let symbol=model.find_symbol(context,scaled as u32);
        let (cum_low,cum_high,model_total)=model.interval(context,symbol);
        if model_total as u64!=total{return Err(CodecError::InvalidEntropyStream)}
        high=low+(range*cum_high as u64/total)-1;
        low=low+range*cum_low as u64/total;
        loop{
            if high<RANGE_HALF{}
            else if low>=RANGE_HALF{code-=RANGE_HALF;low-=RANGE_HALF;high-=RANGE_HALF;}
            else if low>=RANGE_QUARTER&&high<RANGE_THREE_QUARTERS{code-=RANGE_QUARTER;low-=RANGE_QUARTER;high-=RANGE_QUARTER;}
            else{break}
            low<<=1;high=(high<<1)|1;code=(code<<1)|reader.read()? as u64;
        }
        output.push(symbol as u8);
        model.update(context,symbol);
    }
    if !reader.remaining_is_zero()||crc32(&output)!=expected_crc{return Err(CodecError::InvalidEntropyStream)}
    Ok(output)
}

/// Estimated marginal saving helper retained for API compatibility.
pub fn candidate_gain(text:&str,occurrences:usize,code_bytes:usize,dictionary_bytes:usize)->i64{(text.len() as i64*occurrences as i64)-(code_bytes as i64*occurrences as i64)-dictionary_bytes as i64}

#[cfg(test)]
mod jtco_tests {
    use super::*;

    #[test]
    fn jtco_selects_only_candidates_with_real_total_size_savings() {
        let codec = StenoP7::new().set_rans_enabled(false);
        let input = "extraordinary repeated phrase extraordinary repeated phrase extraordinary repeated phrase";
        let chosen = codec.optimize_dictionary(input).unwrap();
        let optimized_cost = codec.complete_archive_cost(input, &chosen).unwrap();
        let empty_cost = codec.complete_archive_cost(input, &[]).unwrap();
        assert!(optimized_cost < empty_cost, "JTCO must improve complete archive size");
        assert!(!chosen.is_empty());
    }

    #[test]
    fn jtco_cost_matches_actual_v2_archive_size() {
        let codec = StenoP7::new();
        let samples = vec![String::new(), "short".to_owned(), "the quick brown fox ".repeat(20)];
        for input in &samples {
            let dict = codec.optimize_dictionary(input).unwrap();
            let expected = codec.complete_archive_cost(input, &dict).unwrap();
            let archive = codec.try_compress(input).unwrap();
            assert_eq!(archive.len(), expected);
        }
    }

    #[test]
    fn jtco_is_deterministic() {
        let codec = StenoP7::new();
        let input = "morphology morphological morphologies; context-aware context-aware phrase phrase";
        assert_eq!(codec.optimize_dictionary(input).unwrap(), codec.optimize_dictionary(input).unwrap());
    }

    #[test]
    fn stage12_beam_search_is_deterministic_and_roundtrips() {
        let codec=StenoP7::new();
        let input=("alpha beta gamma delta alpha beta gamma delta; ".repeat(24))
            + &("alpha beta gamma epsilon alpha beta gamma epsilon; ".repeat(18));
        let first=codec.optimize_dictionary(&input).unwrap();
        let second=codec.optimize_dictionary(&input).unwrap();
        assert_eq!(first,second,"beam search must be deterministic");
        let expected=codec.complete_archive_cost(&input,&first).unwrap();
        let archive=codec.try_compress(&input).unwrap();
        assert_eq!(archive.len(),expected,"JTCO must match serialized archive size");
        assert_eq!(codec.decompress(&archive).unwrap(),input);
    }

    #[test]
    fn stage12_beam_search_never_returns_duplicate_dictionary_entries() {
        let codec=StenoP7::new();
        let input="red blue green red blue green red blue green; ".repeat(20);
        let dict=codec.optimize_dictionary(&input).unwrap();
        let unique:HashSet<String>=dict.iter().cloned().collect();
        assert_eq!(dict.len(),unique.len());
    }

    #[test]
    fn context_rans_roundtrips_varied_byte_patterns() {
        let samples = vec![
            Vec::new(),
            b"the the the the and of to in ".repeat(500),
            b"aaaaaaaa".repeat(25_000),
            "Русский текст 日本語 ไทย العربية\n".as_bytes().repeat(100),
            (0..8192).map(|i| ((i * 73 + i / 7) & 255) as u8).collect(),
        ];
        for sample in samples {
            let (encoded, tables) = rans_context_encode(&sample);
            assert_eq!(rans_context_decode(&encoded, sample.len(), &tables).unwrap(), sample);
        }
    }

    #[test]
    fn v2_static_rans_entropy_mode_roundtrips_archive() {
        let codec = StenoP7::new();
        let text = "static rANS entropy mode must remain independently decodable";
        let mut stream = Vec::new();
        write_literal(text.as_bytes(), &mut stream).unwrap();
        let (payload, frequencies) = rans_encode(&stream);
        let mut archive = Vec::new();
        archive.extend_from_slice(MAGIC);
        archive.push(VERSION_V2);
        put_u32(&mut archive, 0);
        put_u64(&mut archive, stream.len() as u64);
        archive.push(1);
        for frequency in frequencies { put_u16(&mut archive, frequency); }
        put_u64(&mut archive, payload.len() as u64);
        archive.extend_from_slice(&payload);
        assert_eq!(codec.decompress(&archive).unwrap(), text);
    }

    #[test]
    fn v2_context_entropy_mode_roundtrips_archive() {
        let codec = StenoP7::new();
        let text = "context-conditioned entropy coding keeps this text reversible";
        let mut stream = Vec::new();
        write_literal(text.as_bytes(), &mut stream).unwrap();
        let (payload, tables) = rans_context_encode(&stream);
        let mut archive = Vec::new();
        archive.extend_from_slice(MAGIC);
        archive.push(VERSION_V2);
        put_u32(&mut archive, 0);
        put_u64(&mut archive, stream.len() as u64);
        archive.push(2);
        for table in tables { for frequency in table { put_u16(&mut archive, frequency); } }
        put_u64(&mut archive, payload.len() as u64);
        archive.extend_from_slice(&payload);
        assert_eq!(codec.decompress(&archive).unwrap(), text);
    }

    #[test]
    fn adaptive_range_coder_roundtrips_and_detects_corruption() {
        let samples = vec![
            b"a".to_vec(),
            b"the the the the and of to in ".repeat(500),
            b"aaaaaaaa".repeat(25_000),
            "Русский текст 日本語 ไทย العربية\n".as_bytes().repeat(100),
            (0..8192).map(|i| ((i * 73 + i / 7) & 255) as u8).collect(),
        ];
        for sample in samples {
            let encoded = adaptive_range_encode(&sample);
            assert_eq!(adaptive_range_decode(&encoded, sample.len()).unwrap(), sample);
            if !encoded.is_empty() {
                let mut damaged = encoded.clone();
                let last = damaged.len() - 1;
                damaged[last] ^= 1;
                assert!(adaptive_range_decode(&damaged, sample.len()).is_err());
            }
        }
    }

    #[test]
    fn adaptive_mode_is_selected_when_context_repetition_beats_order_zero() {
        let codec = StenoP7::new();
        let text = "a,".repeat(100_000);
        let archive = codec.try_compress(&text).unwrap();
        // Empty dictionary: 4-byte magic + version + 4-byte count + 8-byte
        // token-stream length places the entropy mode at offset 17.
        assert_eq!(archive[17], 3, "adaptive context mode should win for alternating contexts");
        assert_eq!(codec.decompress(&archive).unwrap(), text);
    }

    #[test]
    fn v2_adaptive_entropy_mode_roundtrips_archive() {
        let codec = StenoP7::new();
        let text = "adaptive context model should restore every byte exactly";
        let mut stream = Vec::new();
        write_literal(text.as_bytes(), &mut stream).unwrap();
        let payload = adaptive_range_encode(&stream);
        let mut archive = Vec::new();
        archive.extend_from_slice(MAGIC);
        archive.push(VERSION_V2);
        put_u32(&mut archive, 0);
        put_u64(&mut archive, stream.len() as u64);
        archive.push(3);
        put_u64(&mut archive, payload.len() as u64);
        archive.extend_from_slice(&payload);
        assert_eq!(codec.decompress(&archive).unwrap(), text);
    }

    #[test]
    fn no_space_scripts_generate_reusable_segments() {
        let codec = StenoP7::new();
        let input = "日本語の文章日本語の文章日本語の文章ภาษาไทยภาษาไทยภาษาไทย";
        let candidates = codec.candidate_pool(input);
        assert!(candidates.iter().any(|c| c.text.contains("日本語")));
        assert!(candidates.iter().any(|c| c.text.contains("ภาษา")));
        assert_eq!(codec.decompress(&codec.compress(input)).unwrap(), input);
    }

    #[test]
    fn v1_archive_remains_readable() {
        let codec = StenoP7::new();
        let text = b"legacy v1";
        let mut stream = Vec::new();
        write_literal(text, &mut stream).unwrap();
        let mut archive = Vec::new();
        archive.extend_from_slice(MAGIC);
        archive.push(VERSION_V1);
        put_u32(&mut archive, 0);
        put_u64(&mut archive, stream.len() as u64);
        archive.extend_from_slice(&stream);
        assert_eq!(codec.decompress(&archive).unwrap(), "legacy v1");
    }

    #[test]
    fn mutation_fuzz_decoder_never_panics() {
        let codec = StenoP7::new();
        let seed = codec.compress("STENO-P7 mutation corpus: русский, 日本語, ภาษาไทย, repeated words repeated words");
        let mut state = 0x9E37_79B9u32;
        for iteration in 0..2048usize {
            let mut mutated = seed.clone();
            // Deterministic xorshift mutations make the regression reproducible.
            state ^= state << 13;
            state ^= state >> 17;
            state ^= state << 5;
            match iteration % 4 {
                0 if !mutated.is_empty() => mutated.truncate((state as usize) % mutated.len()),
                1 if !mutated.is_empty() => { let index=(state as usize)%mutated.len(); mutated[index]^=(state>>24) as u8; },
                2 => mutated.extend_from_slice(&(state.to_le_bytes())),
                _ => { if !mutated.is_empty(){let index=(state as usize)%mutated.len(); mutated.remove(index);} }
            }
            let outcome = std::panic::catch_unwind(|| codec.decompress(&mutated));
            assert!(outcome.is_ok(), "decoder panicked on mutation #{iteration}");
        }
    }

    #[test]
    fn configured_output_limit_is_enforced() {
        let input = "output limit regression ".repeat(100);
        let archive = StenoP7::new().compress(&input);
        let mut limits = CodecLimits::default();
        limits.max_output_bytes = 32;
        let restricted = StenoP7::new().with_limits(limits);
        assert!(restricted.decompress(&archive).is_err());
    }

    #[test]
    fn stage9_selects_parse_by_final_entropy_cost_and_roundtrips() {
        let codec = StenoP7::new();
        let input = "the the the repeated phrase repeated phrase — русский текст русский текст 日本語日本語";
        let dict = codec.optimize_dictionary(input).unwrap();
        let ids: HashMap<&str,u64> = dict.iter().enumerate().map(|(i,s)|(s.as_str(),i as u64)).collect();
        let selected = codec.best_parse_stream(input,&ids).unwrap();
        let selected_cost = codec.entropy_payload_cost(&selected);
        for (tw,lw) in [(1usize,1usize),(2,1),(1,2),(3,2)] {
            let mut candidate=Vec::new();
            codec.encode_text_weighted(input,&ids,&mut candidate,tw,lw).unwrap();
            assert!(selected_cost <= codec.entropy_payload_cost(&candidate));
        }
        let archive=codec.try_compress(input).unwrap();
        assert_eq!(codec.decompress(&archive).unwrap(),input);
    }

    #[test]
    fn stage13_contextual_parse_roundtrips_multilingual_script_switches() {
        let codec=StenoP7::new();
        let input=("English repeated words repeated words. Русский повторяющийся текст. 日本語の文章。 ภาษาไทยข้อความ ").repeat(40);
        let dict=codec.optimize_dictionary(&input).unwrap();
        let ids:HashMap<&str,u64>=dict.iter().enumerate().map(|(i,s)|(s.as_str(),i as u64)).collect();
        for penalty in [1usize,2,3]{
            let mut stream=Vec::new();
            codec.encode_text_contextual(&input,&ids,&mut stream,penalty).unwrap();
            let archive=codec.compress(&input);
            assert_eq!(codec.decompress(&archive).unwrap(),input);
            assert!(!stream.is_empty());
        }
    }

    #[test]
    fn stage13_context_candidates_are_compared_by_final_entropy_cost() {
        let codec=StenoP7::new();
        let input=("abc abc Русский текст русский текст 日本語日本語 ").repeat(60);
        let dict=codec.optimize_dictionary(&input).unwrap();
        let ids:HashMap<&str,u64>=dict.iter().enumerate().map(|(i,s)|(s.as_str(),i as u64)).collect();
        let selected=codec.best_parse_stream(&input,&ids).unwrap();
        let selected_cost=codec.entropy_payload_cost(&selected);
        for penalty in [1usize,2,3]{
            let mut candidate=Vec::new();codec.encode_text_contextual(&input,&ids,&mut candidate,penalty).unwrap();
            assert!(selected_cost<=codec.entropy_payload_cost(&candidate));
        }
    }

    #[test]
    fn stage14_markov_model_learns_ordered_script_transitions(){
        let latin=script_class('a') as usize;let cyrillic=script_class('Б') as usize;
        let cjk=script_class('日') as usize;
        let model=ScriptMarkovModel::from_classes(&[latin,latin,cyrillic,cjk,cjk,latin,latin,cyrillic,cjk,cjk]);
        assert!(model.trigram[latin][latin][cyrillic]>0);
        assert!(model.trigram[cyrillic][cjk][cjk]>0);
        assert!(model.penalty(latin,latin,cyrillic)<=model.penalty(latin,latin,cjk));
    }

    #[test]
    fn stage14_predictive_parse_roundtrips_multilingual_text(){
        let codec=StenoP7::new();
        let input=("English English Русский русский 日本語日本語 ภาษาไทยภาษาไทย العربية العربية ").repeat(80);
        let dict=codec.optimize_dictionary(&input).unwrap();
        let ids:HashMap<&str,u64>=dict.iter().enumerate().map(|(i,s)|(s.as_str(),i as u64)).collect();
        for strength in [1usize,2,3]{
            let mut stream=Vec::new();
            codec.encode_text_predictive(&input,&ids,&mut stream,strength).unwrap();
            assert!(!stream.is_empty());
        }
        let archive=codec.compress(&input);
        assert_eq!(codec.decompress(&archive).unwrap(),input);
    }

    #[test]
    fn stage14_predictive_candidates_are_selected_by_final_entropy_cost(){
        let codec=StenoP7::new();
        let input=("alpha beta alpha beta Русский текст Русский текст 日本語日本語 ").repeat(60);
        let dict=codec.optimize_dictionary(&input).unwrap();
        let ids:HashMap<&str,u64>=dict.iter().enumerate().map(|(i,s)|(s.as_str(),i as u64)).collect();
        let selected=codec.best_parse_stream(&input,&ids).unwrap();
        let selected_cost=codec.entropy_payload_cost(&selected);
        for strength in [1usize,2,3]{
            let mut candidate=Vec::new();codec.encode_text_predictive(&input,&ids,&mut candidate,strength).unwrap();
            assert!(selected_cost<=codec.entropy_payload_cost(&candidate));
        }
    }

    #[test]
    fn stage10_frequent_tokens_receive_short_ids() {
        let codec=StenoP7::new();
        let input="alpha beta gamma ".repeat(300)+&"alpha ".repeat(1200);
        let dict=codec.optimize_dictionary(&input).unwrap();
        let ids:HashMap<&str,u64>=dict.iter().enumerate().map(|(i,s)|(s.as_str(),i as u64)).collect();
        let alpha=*ids.get("alpha").expect("frequent token should be selected");
        assert!(alpha<128, "frequent token should receive a one-byte ID");
        assert_eq!(codec.decompress(&codec.compress(&input)).unwrap(),input);
    }

    #[test]
    fn stage10_dictionary_order_is_deterministic() {
        let codec=StenoP7::new();
        let input="редкий частый частый частый 日本語 日本語".repeat(80);
        let a=codec.optimize_dictionary(&input).unwrap();
        let b=codec.optimize_dictionary(&input).unwrap();
        assert_eq!(a,b);
    }

    #[test]
    fn stage11_refinement_keeps_exact_archive_cost_and_roundtrip() {
        let codec=StenoP7::new();
        let input=("adaptive dictionary refinement; overlapping phrases; ".repeat(80)
            + &"refinement refinement adaptive dictionary; ".repeat(60)
            + &"Русский текст и 日本語の文章。".repeat(30));
        let dict=codec.optimize_dictionary(&input).unwrap();
        assert!(dict.len()<=codec.max_dict_entries);
        let predicted=codec.complete_archive_cost(&input,&dict).unwrap();
        let archive=codec.try_compress(&input).unwrap();
        assert_eq!(archive.len(),predicted);
        assert_eq!(codec.decompress(&archive).unwrap(),input);
    }

    #[test]
    fn stage11_refinement_is_deterministic() {
        let codec=StenoP7::new().set_rans_enabled(false);
        let input="one two two three three three; phrase phrase phrase.".repeat(100);
        assert_eq!(codec.optimize_dictionary(&input).unwrap(),
                   codec.optimize_dictionary(&input).unwrap());
    }

}
