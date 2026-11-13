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

// PortOne V2 (ADR-0171). 공식 문서의 흐름: PortOne.requestPayment({storeId, channelKey, paymentId, orderName,
// totalAmount, currency, payMethod, redirectUrl}). 모바일은 redirectUrl 로 넘어가고, 데스크톱은 응답을 돌려준다.
// 응답에 code 가 있으면 실패다. 성공 응답도 믿지 않고 돌아오는 주소로 보내 서버가 조회 API 로 확인한다.
(function () {
  "use strict";
  var box = document.getElementById("pay-portone");
  var button = document.getElementById("pay-portone-open");
  var status = document.getElementById("pay-portone-status");
  if (!box || !button) return;
  var d = box.dataset;
  function say(text) {
    if (status) status.textContent = text;
  }
  if (!window.PortOne || typeof window.PortOne.requestPayment !== "function") {
    say("결제사 스크립트를 불러오지 못했습니다. 잠시 뒤 새로 고쳐 보세요.");
    return;
  }
  function back(paymentId) {
    var sep = d.redirectUrl.indexOf("?") < 0 ? "?" : "&";
    window.location.href = d.redirectUrl + sep + "paymentId=" + encodeURIComponent(paymentId);
  }
  async function open() {
    button.disabled = true;
    say("결제 창을 여는 중입니다.");
    var req = {
      storeId: d.storeId,
      channelKey: d.channelKey,
      paymentId: d.paymentId,
      orderName: d.orderName,
      totalAmount: Number(d.amount),
      currency: "CURRENCY_KRW",
      payMethod: "CARD",
      redirectUrl: d.redirectUrl
    };
    var customer = {};
    if (d.customerName) customer.fullName = d.customerName;
    if (d.customerEmail) customer.email = d.customerEmail;
    if (d.customerPhone) customer.phoneNumber = d.customerPhone;
    if (Object.keys(customer).length) req.customer = customer;
    try {
      var res = await window.PortOne.requestPayment(req);
      if (!res) return; // 돌아오는 주소로 넘어가는 중
      if (res.code != null) {
        say(res.message || "결제를 마치지 못했습니다.");
        button.disabled = false;
        return;
      }
      say("결제를 확인하고 있습니다.");
      back(res.paymentId || d.paymentId);
    } catch (e) {
      say((e && e.message) || "결제 창을 열지 못했습니다.");
      button.disabled = false;
    }
  }
  button.addEventListener("click", open);
  button.disabled = false;
  say("");
})();
