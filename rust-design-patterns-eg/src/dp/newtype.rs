// Gotta define those newtype structs for our different IDs
#[derive(Debug, PartialEq, Eq)]
struct UserId(u64);
#[derive(Debug, PartialEq, Eq)]
struct ProductId(u64);
#[derive(Debug, PartialEq, Eq)]
struct OrderId(u64);
// A function that, you know, *really* needs a UserId
fn get_user_profile(user_id: UserId) {
    println!("Fetching profile for user ID: {:?}", user_id);
}
// And this one, it *really* needs a ProductId
fn get_product_details(product_id: ProductId) {
    println!("Fetching details for product ID: {:?}", product_id);
}
fn main() {
    let my_user_id = UserId(12345);
    let my_product_id = ProductId(67890);
    let my_order_id = OrderId(98765);
    get_user_profile(my_user_id);
    get_product_details(my_product_id);
    // See? This would actually cause a compile-time error! How neat is that?
    // get_user_profile(my_product_id);
    // ^ expected struct `UserId`, found struct `ProductId`
    println!("Order ID: {:?}", my_order_id);
}
