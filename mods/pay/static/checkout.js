// 토스페이먼츠 결제창형 결제위젯 (ADR-0166). 공식 예제(tosspayments-sample, widget/checkout-window.html)의 흐름:
// TossPayments(clientKey).widgets({customerKey}) → setAmount → renderPaymentWindow → paymentRequest 때 requestPayment.
// 금액·주문 번호는 서버가 정해 data- 속성으로 준 값이다. 서버가 돌아온 금액을 다시 대조한다.
(function () {
  "use strict";
  var box = document.getElementById("pay-toss");
  var button = document.getElementById("pay-toss-open");
  var status = document.getElementById("pay-toss-status");
  if (!box || !button) return;
  var d = box.dataset;
  function say(text) {
    if (status) status.textContent = text;
  }
  if (typeof window.TossPayments !== "function") {
    say("결제사 스크립트를 불러오지 못했습니다. 잠시 뒤 새로 고쳐 보세요.");
    return;
  }
  var customerKey = d.customerKey || window.TossPayments.ANONYMOUS;
  var widgets = window.TossPayments(d.clientKey).widgets({ customerKey: customerKey });
  function request() {
    var req = {
      orderId: d.orderId,
      orderName: d.orderName,
      successUrl: d.successUrl,
      failUrl: d.failUrl
    };
    if (d.customerName) req.customerName = d.customerName;
    if (d.customerEmail) req.customerEmail = d.customerEmail;
    if (d.customerPhone) req.customerMobilePhone = d.customerPhone;
    return req;
  }
  async function open() {
    button.disabled = true;
    say("결제 창을 여는 중입니다.");
    try {
      await widgets.setAmount({ currency: "KRW", value: Number(d.amount) });
      var win = await widgets.renderPaymentWindow({
        variantKey: { paymentMethod: "DEFAULT", agreement: "AGREEMENT" }
      });
      win.on("paymentRequest", async function () {
        try {
          await widgets.requestPayment(request());
        } catch (e) {
          say((e && e.message) || "결제를 요청하지 못했습니다.");
          button.disabled = false;
        }
        await win.destroy();
      });
      say("결제 창에서 결제 수단을 고르세요.");
    } catch (e) {
      say((e && e.message) || "결제 창을 열지 못했습니다.");
      button.disabled = false;
    }
  }
  button.addEventListener("click", open);
  button.disabled = false;
  say("");
})();
