use crate::crypto_helpers::{eddsa_sign, get_pkh, get_private_key, get_pubkey, get_pubkey_from_privkey, Hasher};
use crate::interface::*;
use crate::*;
use arrayvec::ArrayVec;
use core::fmt::Write;
use ledger_log::{info};
use ledger_parser_combinators::interp_parser::{
    Action, DefaultInterp, DropInterp, InterpParser, ObserveLengthedBytes, SubInterp, OOB, set_from_thunk
};
use ledger_parser_combinators::json::Json;
use ledger_parser_combinators::core_parsers::Alt;
use prompts_ui::{write_scroller, final_accept_prompt};

use ledger_parser_combinators::define_json_struct_interp;
use ledger_parser_combinators::json::*;
use ledger_parser_combinators::json_interp::*;
use core::convert::TryFrom;
use core::str::from_utf8;

// A couple type ascription functions to help the compiler along.
const fn mkfn<A,B,C>(q: fn(&A,&mut B)->C) -> fn(&A,&mut B)->C {
  q
}
const fn mkvfn<A,C>(q: fn(&A,&mut Option<()>)->C) -> fn(&A,&mut Option<()>)->C {
  q
}

pub type GetAddressImplT = impl InterpParser<Bip32Key, Returning = ArrayVec<u8, 128_usize>>;
pub const GET_ADDRESS_IMPL: GetAddressImplT =
    Action(SubInterp(DefaultInterp), mkfn(|path: &ArrayVec<u32, 10>, destination: &mut Option<ArrayVec<u8, 128>>| {
        let key = get_pubkey(&path).ok()?;

        let pkh = get_pkh(key);

        write_scroller("Provide Public Key", |w| Ok(write!(w, "{}", pkh)?))?;

        final_accept_prompt(&[])?;

        *destination=Some(ArrayVec::new());
        destination.as_mut()?.try_push(u8::try_from(key.W_len).ok()?).ok()?;
        destination.as_mut()?.try_extend_from_slice(&key.W[1..key.W_len as usize]).ok()?;
        Some(())
    }));

pub type SignImplT = impl InterpParser<SignParameters, Returning = ArrayVec<u8, 128_usize>>;

#[derive(Debug)]
enum CommandData {
    Known,
    Unknown
}

#[derive(PartialEq, Debug)]
enum CapabilityCoverage {
    Full,
    NotFull
}

impl Summable<CapabilityCoverage> for CapabilityCoverage {
    fn zero() -> Self { CapabilityCoverage::Full }
    fn add_and_set(&mut self, other: &CapabilityCoverage) {
        match other {
            CapabilityCoverage::Full => { }
            CapabilityCoverage::NotFull => { *self = CapabilityCoverage::NotFull }
        }
    }
}

fn prompt_cross_chain_from_str(s: &str) -> Option<Option<()>> {
    let (from_field, rest1) = s.strip_prefix("(coin.transfer-crosschain \"")?.split_once("\" \"")?;
    let (to_field, rest2) = rest1.split_once("\" (read-keyset \"ks\") \"")?;
    let (to_chain, rest3) = rest2.split_once("\" ")?;
    let (amount, rest4) = rest3.split_once(")")?;
    
    if rest4 != "" || from_field.contains('"') || to_field.contains('"') || to_chain.contains('"') || amount.contains(|c: char| !c.is_ascii_digit() && c != '.') {
        None
    } else {
        Some(write_scroller("Transfer", |w| Ok(write!(w, "Cross-chain {} from {} to {} on chain {}", amount, from_field, to_field, to_chain)?)))
    }
}

pub static SIGN_IMPL: SignImplT = Action(
    (
        Action(
            // Calculate the hash of the transaction
            ObserveLengthedBytes(
                Hasher::new,
                Hasher::update,
                Json(Action(Preaction( || -> Option<()> { write_scroller("Signing", |w| Ok(write!(w, "Transaction")?)) } , KadenaCmdInterp {
                    field_nonce: DropInterp,
                    field_meta: MetaInterp {
                        field_chain_id: Action(JsonStringAccumulate::<32>, mkvfn(|chain: &ArrayVec<u8, 32>, _| -> Option<()> {
                                write_scroller("On Chain", |w| Ok(write!(w, "{}", from_utf8(chain.as_slice()).ok()?)?))
                        })),
                        field_sender: DropInterp,
                        field_gas_limit: DropInterp,
                        field_gas_price: DropInterp,
                        field_ttl: DropInterp,
                        field_creation_time: DropInterp
                    },
                    field_payload: PayloadInterp {
                        field_exec: CommandInterp {
                            field_code: Action(OrDrop(JsonStringAccumulate::<600>), mkfn(|cmd_opt: &Option<ArrayVec<u8, 600>>, dest: &mut Option<CommandData> | { 
                                // The length of 600 above is somewhat arbitrary, but should cover
                                // two k:-addresses and a reasonable number of digits for the
                                // amount.
                                match cmd_opt {
                                    Some(cmd) => {
                                        match prompt_cross_chain_from_str(from_utf8(cmd.as_slice()).ok()?) {
                                            Some(rv) => { rv?; *dest=Some(CommandData::Known); }
                                            None => { *dest = Some(CommandData::Unknown); }
                                        }
                                    }
                                    None => { *dest = Some(CommandData::Unknown); }
                                }
                                Some(())
                            })),
                            field_data: DropInterp
                        }},
                    field_signers: SubInterpM::<_, CapabilityCoverage>::new(Action(Preaction(
                            || -> Option<()> {
                                write_scroller("Requiring", |w| Ok(write!(w, "Capabilities")?))
                            },
                            SignerInterp {
                        field_scheme: DropInterp,
                        field_pub_key: Action(JsonStringAccumulate::<64>, mkvfn(|key : &ArrayVec<u8, 64>, _: &mut Option<()>| -> Option<()> {
                            write_scroller("Of Key", |w| Ok(write!(w, "{}", from_utf8(key.as_slice())?)?))
                        })),
                        field_addr: DropInterp,
                        field_clist: SubInterpM::<_, Count>::new(Action(
                                KadenaCapabilityInterp {
                                    field_args: KadenaCapabilityArgsInterp,
                                    field_name: JsonStringAccumulate::<14>
                                },
                            mkvfn(|cap : &KadenaCapability<Option<<KadenaCapabilityArgsInterp as JsonInterp<JsonArray<JsonAny>>>::Returning>, Option<ArrayVec<u8, 14>>>, destination| {
                                let name = cap.field_name.as_ref()?.as_slice();
                                trace!("Prompting for capability");
                                *destination = Some(());
                                match cap.field_args.as_ref()? {
                                    (None, None, None) if name == b"coin.GAS" => {
                                        write_scroller("Paying Gas", |w| Ok(write!(w, " ")?))?;
                                        trace!("Accepted gas");
                                    }
                                    _ if name == b"coin.GAS" => { return None; }
                                    (Some(Some(acct)), None, None) if name == b"coin.ROTATE" => {
                                        write_scroller("Rotate for account", |w| Ok(write!(w, "{}", from_utf8(acct.as_slice())?)?))?;
                                    }
                                    _ if name == b"coin.ROTATE" => { return None; }
                                    (Some(Some(sender)), Some(Some(receiver)), Some(Some(amount))) if name == b"coin.TRANSFER" => {
                                        write_scroller("Transfer", |w| Ok(write!(w, "{} from {} to {}", from_utf8(amount.as_slice())?, from_utf8(sender.as_slice())?, from_utf8(receiver.as_slice())?)?))?;
                                    }
                                    _ if name == b"coin.TRANSFER" => { return None; }
                                    _ => { return None; } // Change this to allow unknown capabilities.
                                }
                                Some(())
                            }),
                        )),
                    }),
                        mkfn(|signer: &Signer<_,_,_, Option<Count>>, dest: &mut Option<CapabilityCoverage> | {
                            *dest = Some(match signer.field_clist {
                                Some(Count(n)) if n > 0 => CapabilityCoverage::Full,
                                _ => CapabilityCoverage::NotFull,
                            });
                            Some(())
                        })),
                        ),
                    field_network_id: Action(JsonStringAccumulate::<32>, mkvfn(|net: &ArrayVec<u8, 32>, dest: &mut Option<()>| {
                        *dest = Some(());
                        write_scroller("On Network", |w| Ok(write!(w, "{}", from_utf8(net.as_slice())?)?))
                    }))
                }),
                mkvfn(|cmd : &KadenaCmd<_,_,Option<CapabilityCoverage>,Option<Payload<Option<Command<_,Option<_>>>>>,_>, _| { 
                    match (|| cmd.field_payload.as_ref()?.field_exec.as_ref()?.field_code.as_ref() )() {
                        Some(CommandData::Known) => { }
                        _ => {
                            match cmd.field_signers.as_ref() {
                                Some(CapabilityCoverage::Full) => { }
                                _ => {
                                    write_scroller("WARNING", |w| Ok(write!(w, "UNSAFE TRANSACTION. This transaction's code was not recognized and does not limit capabilities for all signers. Signing this transaction may make arbitrary actions on the chain including loss of all funds.")?))?;
                                }
                            }
                        }
                    }
                    Some(())
                })
                )),
            true),
            // Ask the user if they accept the transaction body's hash
            mkfn(|(_, mut hash): &(_, Hasher), destination: &mut Option<[u8; 32]>| {
                let the_hash = hash.finalize();
                write_scroller("Transaction hash", |w| Ok(write!(w, "{}", the_hash)?))?;
                *destination=Some(the_hash.0.into());
                Some(())
            }),
        ),
        Action(
            SubInterp(DefaultInterp),
            // And ask the user if this is the key the meant to sign with:
            mkfn(|path: &ArrayVec<u32, 10>, destination: &mut _| {
                // Mutable because of some awkwardness with the C api.
                let mut privkey = get_private_key(&path).ok()?;
                let pubkey = get_pubkey_from_privkey(&mut privkey).ok()?;
                let pkh = get_pkh(pubkey);

                write_scroller("Sign for Address", |w| Ok(write!(w, "{}", pkh)?))?;
                *destination = Some(privkey);
                Some(())
            }),
        ),
    ),
    mkfn(|(hash, key): &(Option<[u8; 32]>, Option<_>), destination: &mut _| {
        final_accept_prompt(&[&"Sign Transaction?"])?;

        // By the time we get here, we've approved and just need to do the signature.
        let sig = eddsa_sign(&hash.as_ref()?[..], key.as_ref()?)?;
        let mut rv = ArrayVec::<u8, 128>::new();
        rv.try_extend_from_slice(&sig.0[..]).ok()?;
        *destination = Some(rv);
        Some(())
    }),
);

pub struct KadenaCapabilityArgsInterp;

#[derive(Debug)]
pub enum KadenaCapabilityArgsInterpState {
    Start,
    Begin,
    FirstArgument(<OrDropAny<JsonStringAccumulate<128>> as JsonInterp<Alt<JsonString, JsonAny>>>::State),
    FirstValueSep,
    SecondArgument(<OrDropAny<JsonStringAccumulate<128>> as JsonInterp<Alt<JsonString, JsonAny>>>::State),
    SecondValueSep,
    ThirdArgument(<OrDropAny<JsonStringAccumulate<20>> as JsonInterp<Alt<JsonNumber, JsonAny>>>::State),
    ThirdValueSep,
    FallbackValue(<DropInterp as JsonInterp<JsonAny>>::State),
    FallbackValueSep
}

impl JsonInterp<JsonArray<JsonAny>> for KadenaCapabilityArgsInterp {
    type State = (KadenaCapabilityArgsInterpState, Option<<DropInterp as JsonInterp<JsonAny>>::Returning>);
    type Returning = ( Option<Option<ArrayVec<u8, 128>>>, Option<Option<ArrayVec<u8, 128>>>, Option<Option<ArrayVec<u8, 20>>> );
    fn init(&self) -> Self::State {
        (KadenaCapabilityArgsInterpState::Start, None)
    }
    #[inline(never)]
    fn parse<'a, 'b>(&self, (ref mut state, ref mut scratch): &'b mut Self::State, token: JsonToken<'a>, destination: &mut Option<Self::Returning>) -> Result<(), Option<OOB>> {
        let str_interp = OrDropAny(JsonStringAccumulate::<128>);
        let dec_interp = OrDropAny(JsonStringAccumulate::<20>);
        loop {
            use KadenaCapabilityArgsInterpState::*;
            match state {
                Start if token == JsonToken::BeginArray => {
                    set_from_thunk(destination, || Some((None, None, None)));
                    set_from_thunk(state, || Begin);
                }
                Begin if token == JsonToken::EndArray => {
                    return Ok(());
                }
                Begin => {
                    set_from_thunk(state, || FirstArgument(<OrDropAny<JsonStringAccumulate<128>> as JsonInterp<Alt<JsonString, JsonAny>>>::init(&str_interp)));
                    continue;
                }
                FirstArgument(ref mut s) => {
                    <OrDropAny<JsonStringAccumulate<128>> as JsonInterp<Alt<JsonString, JsonAny>>>::parse(&str_interp, s, token, &mut destination.as_mut().ok_or(Some(OOB::Reject))?.0)?;
                    set_from_thunk(state, || FirstValueSep);
                }
                FirstValueSep if token == JsonToken::ValueSeparator => {
                    set_from_thunk(state, || SecondArgument(<OrDropAny<JsonStringAccumulate<128>> as JsonInterp<Alt<JsonString, JsonAny>>>::init(&str_interp)));
                }
                FirstValueSep if token == JsonToken::EndArray => return Ok(()),
                SecondArgument(ref mut s) => {
                    <OrDropAny<JsonStringAccumulate<128>> as JsonInterp<Alt<JsonString, JsonAny>>>::parse(&str_interp, s, token, &mut destination.as_mut().ok_or(Some(OOB::Reject))?.1)?;
                    set_from_thunk(state, || SecondValueSep);
                }
                SecondValueSep if token == JsonToken::ValueSeparator => {
                    set_from_thunk(state, || ThirdArgument(<OrDropAny<JsonStringAccumulate<20>> as JsonInterp<Alt<JsonNumber, JsonAny>>>::init(&dec_interp)));
                }
                SecondValueSep if token == JsonToken::EndArray => return Ok(()),
                ThirdArgument(ref mut s) => {
                    <OrDropAny<JsonStringAccumulate<20>> as JsonInterp<Alt<JsonNumber, JsonAny>>>::parse(&dec_interp, s, token, &mut destination.as_mut().ok_or(Some(OOB::Reject))?.2)?;
                    set_from_thunk(state, || FirstValueSep);
                }
                ThirdValueSep if token == JsonToken::EndArray => {
                    return Ok(());
                }
                ThirdValueSep if token == JsonToken::ValueSeparator => {
                    set_from_thunk(destination, || None);
                    set_from_thunk(state, || FallbackValue(<DropInterp as JsonInterp<JsonAny>>::init(&DropInterp)));
                }
                FallbackValue(ref mut s) => {
                    <DropInterp as JsonInterp<JsonAny>>::parse(&DropInterp, s, token, scratch)?;
                    set_from_thunk(state, || FallbackValueSep);
                }
                FallbackValueSep if token == JsonToken::ValueSeparator => {
                    set_from_thunk(state, || FallbackValue(<DropInterp as JsonInterp<JsonAny>>::init(&DropInterp)));
                }
                FallbackValueSep if token == JsonToken::EndArray => {
                    return Ok(());
                }
                _ => return Err(Some(OOB::Reject))
            }
            break Err(None)
        }
    }
}

// The global parser state enum; any parser above that'll be used as the implementation for an APDU
// must have a field here.

pub enum ParsersState {
    NoState,
    GetAddressState(<GetAddressImplT as InterpParser<Bip32Key>>::State),
    SignState(<SignImplT as InterpParser<SignParameters>>::State),
}

pub fn reset_parsers_state(state: &mut ParsersState) {
    *state = ParsersState::NoState;
}

meta_definition!{}
kadena_capability_definition!{}
signer_definition!{}
payload_definition!{}
command_definition!{}
kadena_cmd_definition!{}

#[inline(never)]
pub fn get_get_address_state(
    s: &mut ParsersState,
) -> &mut <GetAddressImplT as InterpParser<Bip32Key>>::State {
    match s {
        ParsersState::GetAddressState(_) => {}
        _ => {
            info!("Non-same state found; initializing state.");
            *s = ParsersState::GetAddressState(<GetAddressImplT as InterpParser<Bip32Key>>::init(
                &GET_ADDRESS_IMPL,
            ));
        }
    }
    match s {
        ParsersState::GetAddressState(ref mut a) => a,
        _ => {
            panic!("")
        }
    }
}

#[inline(never)]
pub fn get_sign_state(
    s: &mut ParsersState,
) -> &mut <SignImplT as InterpParser<SignParameters>>::State {
    match s {
        ParsersState::SignState(_) => {}
        _ => {
            info!("Non-same state found; initializing state.");
            *s = ParsersState::SignState(<SignImplT as InterpParser<SignParameters>>::init(
                &SIGN_IMPL,
            ));
        }
    }
    match s {
        ParsersState::SignState(ref mut a) => a,
        _ => {
            panic!("")
        }
    }
}
