/// 식별자 생성기. 프로덕션은 ULID, 테스트는 고정 시퀀스를 주입한다(결정성).
pub trait IdGen {
    fn new_id(&self) -> String;
}

pub struct UlidGen;

impl IdGen for UlidGen {
    fn new_id(&self) -> String {
        ulid::Ulid::new().to_string()
    }
}
