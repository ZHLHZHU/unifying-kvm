//! Board RGB indicator: each target slot has a fixed color; pairing blinks it.
pub const RED:u8=1;pub const GREEN:u8=2;pub const BLUE:u8=4;pub const WHITE:u8=RED|GREEN|BLUE;
const PAIRING_HALF_PERIOD_US:u64=250_000;
/// Faster than pairing so "reaching the receiver" and "pairing" look different.
const CONNECTING_HALF_PERIOD_US:u64=100_000;
#[derive(Clone,Copy,Debug,PartialEq,Eq)]
pub enum Indicator { Off, Slot(u8), Connecting(u8), Pairing(u8), Fault }
pub const fn slot_color(slot:u8)->u8{match slot{0=>GREEN,1=>BLUE,_=>RED}}
/// RGB mask (bit0 R, bit1 G, bit2 B) to show at `now`.
pub fn rgb(indicator:Indicator,now:u64)->u8{
    match indicator{
        Indicator::Off=>0,
        Indicator::Slot(slot)=>slot_color(slot),
        Indicator::Connecting(slot)=>if (now/CONNECTING_HALF_PERIOD_US)%2==0{slot_color(slot)}else{0},
        Indicator::Pairing(slot)=>if (now/PAIRING_HALF_PERIOD_US)%2==0{slot_color(slot)}else{0},
        Indicator::Fault=>WHITE,
    }
}
#[cfg(test)]mod tests{
    use super::*;
    #[test]fn slots_are_distinct_from_each_other_and_fault(){
        let colors=[slot_color(0),slot_color(1),slot_color(2),WHITE];
        for (i,a) in colors.iter().enumerate(){for b in &colors[i+1..]{assert_ne!(a,b);}}
    }
    #[test]fn pairing_blinks_its_slot_color(){
        let p=Indicator::Pairing(1);
        assert_eq!(rgb(p,0),BLUE);assert_eq!(rgb(p,PAIRING_HALF_PERIOD_US-1),BLUE);
        assert_eq!(rgb(p,PAIRING_HALF_PERIOD_US),0);assert_eq!(rgb(p,2*PAIRING_HALF_PERIOD_US),BLUE);
    }
    #[test]fn connecting_blinks_faster_than_pairing(){
        let c=Indicator::Connecting(0);
        assert_eq!(rgb(c,0),GREEN);assert_eq!(rgb(c,CONNECTING_HALF_PERIOD_US),0);
        assert_eq!(rgb(Indicator::Pairing(0),CONNECTING_HALF_PERIOD_US),GREEN);
    }
}
